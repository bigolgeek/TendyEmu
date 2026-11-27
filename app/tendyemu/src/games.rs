//! Per-game overrides (DESIGN §6.2, SPEC §6): `<system>/games/<crc32>.toml`, read on open and
//! written when a row changes. The file holds system-section keys only, so a game inherits the
//! system section and `[app]` for everything it does not name.

use std::io;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::SystemTime;

use tendy_core::SystemKind;

use crate::app::App;
use crate::config;
use crate::menu::{Row, RowAct, RowKind};
use crate::paths::Paths;
use crate::settings::Direction;

/// The keys every system's games may override: the shared picture keys.
pub const KEYS: [&str; 2] = ["scale", "fullscreen"];

/// The three keys the systems whose section has `integer_scale` may add.
const KEYS_INTEGER: [&str; 3] = ["scale", "fullscreen", "integer_scale"];

/// The keys a game of `system` may override: the picture every core shares (the pace and palettes
/// join with the system parity rows, #25–#27). The GBA's section has no integer-scale key.
pub fn keys(system: SystemKind) -> &'static [&'static str] {
    if system == SystemKind::Gba {
        &KEYS
    } else {
        &KEYS_INTEGER
    }
}

/// The override file: `<system>/games/<crc32>.toml`, lowercase hex as the #21 row shows it.
pub fn path(paths: &Paths, system: SystemKind, crc32: u32) -> PathBuf {
    paths.games_dir(system).join(format!("{crc32:08x}.toml"))
}

/// One parsed file, keyed by its identity and stamp, so reading a value per frame does not mean a
/// disk read per frame.
struct Cached {
    root: PathBuf,
    system: SystemKind,
    crc32: u32,
    stamp: (Option<SystemTime>, u64),
    config: config::Config,
}

static CACHE: Mutex<Option<Cached>> = Mutex::new(None);

fn stamp(file: &std::path::Path) -> (Option<SystemTime>, u64) {
    match std::fs::metadata(file) {
        Ok(meta) => (meta.modified().ok(), meta.len()),
        Err(_) => (None, 0),
    }
}

/// The overrides the file holds. A missing or malformed file is no overrides.
pub fn load(paths: &Paths, system: SystemKind, crc32: u32) -> config::Config {
    let file = path(paths, system, crc32);
    let stamp = stamp(&file);
    {
        let cache = CACHE.lock().unwrap_or_else(|poison| poison.into_inner());
        if let Some(cached) = cache.as_ref()
            && cached.root == paths.root()
            && cached.system == system
            && cached.crc32 == crc32
            && cached.stamp == stamp
        {
            return cached.config.clone();
        }
    }
    let config = match std::fs::read_to_string(&file) {
        Ok(text) => match config::parse_named(&text, system.key(), "games file") {
            Ok(loaded) => loaded.config,
            Err(_) => config::Config::new(),
        },
        Err(_) => config::Config::new(),
    };
    let mut cache = CACHE.lock().unwrap_or_else(|poison| poison.into_inner());
    *cache = Some(Cached {
        root: paths.root().to_path_buf(),
        system,
        crc32,
        stamp,
        config: config.clone(),
    });
    config
}

/// One override, when the game names it.
pub fn value(paths: &Paths, system: SystemKind, crc32: u32, key: &str) -> Option<String> {
    load(paths, system, crc32)
        .get(system.key(), key)
        .map(str::to_string)
}

/// Writes one override.
pub fn set(
    paths: &Paths,
    system: SystemKind,
    crc32: u32,
    key: &str,
    value: &str,
) -> io::Result<()> {
    let mut config = load(paths, system, crc32);
    config.set(system.key(), key, value);
    write(paths, system, crc32, &config)
}

fn write(paths: &Paths, system: SystemKind, crc32: u32, config: &config::Config) -> io::Result<()> {
    let file = path(paths, system, crc32);
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let result =
        crate::battery::write_atomic(&file, config::to_flat_text(config, system.key()).as_bytes());
    if result.is_ok() {
        forget(paths, system, crc32);
    }
    result
}

/// Drops the game's overrides, if it had any.
pub fn clear(paths: &Paths, system: SystemKind, crc32: u32) -> io::Result<()> {
    match std::fs::remove_file(path(paths, system, crc32)) {
        Err(why) if why.kind() != io::ErrorKind::NotFound => Err(why),
        _ => {
            forget(paths, system, crc32);
            Ok(())
        }
    }
}

/// Drops the cache entry after our own write, which the clock's stamp alone may not reveal.
fn forget(paths: &Paths, system: SystemKind, crc32: u32) {
    let mut cache = CACHE.lock().unwrap_or_else(|poison| poison.into_inner());
    if cache.as_ref().is_some_and(|cached| {
        cached.root == paths.root() && cached.system == system && cached.crc32 == crc32
    }) {
        *cache = None;
    }
}

/// The running game's system and identity, when a game runs.
fn running(app: &App) -> Option<(SystemKind, u32)> {
    let system = app.system()?;
    let machine = app.machine()?;
    Some((system, machine.cart_identity().crc32))
}

/// Steps one integer value inside its legible range.
fn stepped(current: &str, step: i64, low: i64, high: i64) -> String {
    let value = current.parse::<i64>().unwrap_or(low);
    value.saturating_add(step).clamp(low, high).to_string()
}

/// Flips one boolean value.
fn flipped(current: &str) -> &'static str {
    if current == "true" { "false" } else { "true" }
}

/// Writes one override, starting from what the system runs with when the game names nothing.
pub fn on_row(app: &mut App, index: usize, act: RowAct) -> bool {
    let Some((system, crc32)) = running(app) else {
        return false;
    };
    let step = match act {
        RowAct::Step(Direction::Left) => -1,
        RowAct::Step(Direction::Right) | RowAct::Activate => 1,
        RowAct::Key(_) => return false,
    };
    // Row 0 is the info row; the keys follow it, and "Clear this game's settings" comes last.
    let Some(key_index) = index.checked_sub(1) else {
        return false;
    };
    let keys = keys(system);
    if key_index >= keys.len() {
        if key_index == keys.len()
            && !load(app.paths(), system, crc32).is_empty_section(system.key())
        {
            if let Err(why) = clear(app.paths(), system, crc32) {
                app.show_notice(format!("per-game settings not cleared: {why}"), true);
            }
            return true;
        }
        return false;
    }
    let key = keys[key_index];
    let current = value(app.paths(), system, crc32, key)
        .or_else(|| {
            config::load(app.paths())
                .config
                .value(system, key)
                .map(str::to_string)
        })
        .unwrap_or_default();
    let next = match key {
        "scale" => stepped(&current, step, 1, 6),
        _ => flipped(&current).to_string(),
    };
    match set(app.paths(), system, crc32, key, &next) {
        Ok(()) => true,
        Err(why) => {
            app.show_notice(format!("per-game settings not written: {why}"), true);
            false
        }
    }
}

/// The drawer's `THIS GAME` rows: the override file's picture keys, and a way back to the
/// system's values.
pub fn rows(app: &App) -> Vec<Row> {
    let Some((system, crc32)) = running(app) else {
        return Vec::new();
    };
    let title = app
        .machine()
        .map_or_else(String::new, |machine| machine.cart_identity().title);
    let mut rows = vec![Row::info(
        "Game",
        format!("{title} [{crc32:08x}]"),
        "The key of this game's settings file",
    )];
    let overrides = load(app.paths(), system, crc32);
    for key in keys(system) {
        let stored = overrides.get(system.key(), key);
        let label = match *key {
            "scale" => "Scale",
            "fullscreen" => "Fullscreen",
            _ => "Integer scale",
        };
        let (kind, value) = match *key {
            "scale" => (RowKind::Choice, stored.map(str::to_string)),
            _ => (RowKind::Toggle, stored.map(str::to_string)),
        };
        rows.push(Row {
            label: label.to_string(),
            value: value.unwrap_or_else(|| "(inherit)".to_string()),
            kind,
            desc: "Wins over the system's setting",
            hotkey: "",
            requires: None,
            act: None,
        });
    }
    if !overrides.is_empty_section(system.key()) {
        rows.push(Row {
            label: "Clear this game's settings".to_string(),
            value: String::new(),
            kind: RowKind::Action,
            desc: "Back to the system's values",
            hotkey: "",
            requires: None,
            act: None,
        });
    }
    rows
}

/// Nothing to do per frame; overrides are read where they are used.
pub fn on_frame(_app: &mut App) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_step_stays_inside_the_legible_range() {
        assert_eq!(stepped("1", -1, 1, 6), "1");
        assert_eq!(stepped("6", 1, 1, 6), "6");
        assert_eq!(stepped("3", 1, 1, 6), "4");
    }

    #[test]
    fn a_toggle_flips_both_ways() {
        assert_eq!(flipped("true"), "false");
        assert_eq!(flipped("false"), "true");
    }

    #[test]
    fn the_file_is_named_after_the_crc32() {
        let paths = Paths::new("scratch");
        assert_eq!(
            path(&paths, SystemKind::Snes, 0x1A2B_3C4D),
            std::path::PathBuf::from("scratch")
                .join("snes")
                .join("games")
                .join("1a2b3c4d.toml")
        );
    }

    #[test]
    fn a_file_holds_only_what_the_game_names() {
        let root = std::env::temp_dir().join(format!("tendyemu-games-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let paths = Paths::new(&root);
        set(&paths, SystemKind::Gb, 7, "scale", "6").unwrap();
        set(&paths, SystemKind::Gb, 7, "fullscreen", "true").unwrap();
        assert_eq!(
            value(&paths, SystemKind::Gb, 7, "scale").as_deref(),
            Some("6")
        );
        assert_eq!(
            value(&paths, SystemKind::Gb, 7, "fullscreen").as_deref(),
            Some("true")
        );
        assert_eq!(value(&paths, SystemKind::Gb, 7, "integer_scale"), None);
        assert_eq!(value(&paths, SystemKind::Gb, 8, "scale"), None);
        let text = std::fs::read_to_string(path(&paths, SystemKind::Gb, 7)).unwrap();
        assert!(text.contains("scale = 6"), "{text}");
        assert!(!text.contains('['), "{text}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn every_override_key_is_a_key_of_its_system() {
        for system in SystemKind::ALL {
            for key in keys(system) {
                assert!(
                    config::Config::new().value(system, key).is_some(),
                    "{} has no `{key}`",
                    system.key()
                );
            }
        }
        assert_eq!(keys(SystemKind::Gba), &["scale", "fullscreen"]);
    }
}
