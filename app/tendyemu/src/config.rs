//! `config.toml` (DESIGN §6.2): one file, a section per system, hand-written, atomic on write.
//!
//! The source apps wrote keys like `scale = 3` at the top of their own file. Here every key lives
//! in `[app]` when every system shares it, and in `[gb]`, `[gba]`, `[nes]` or `[snes]` when it is
//! one system's. A system section may repeat an `[app]` key and wins for that system. A per-game
//! file (`<system>/games/<crc32>.toml`, [`crate::games`]) wins over both.
//!
//! The known keys are the source apps' own key lists — same names, same defaults — plus the keys
//! SPEC §7 adds (`bios`, `fds_bios`, `sgb_sound_rom`). A key this build does not know is one notice
//! and then ignored; a value that is not a whole number or a boolean is an error naming the file
//! and its line, because a silent typo there would quietly run the wrong settings.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

use tendy_core::SystemKind;

use crate::app::App;
use crate::battery::write_atomic;
use crate::menu::{Row, RowAct};
use crate::paths::Paths;

/// The section every system shares.
pub const APP: &str = "app";

/// The system sections, in write order.
pub const SYSTEMS: [&str; 4] = ["gb", "gba", "nes", "snes"];

/// Every section name the file may hold.
pub const SECTIONS: [&str; 5] = [APP, "gb", "gba", "nes", "snes"];

/// How a value is read and written back.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Bool,
    Int,
    /// A quoted word or path.
    Text,
    /// One of a fixed set of words; anything else takes the default, as the sources do for
    /// `console`.
    Choice(&'static [&'static str]),
}

/// One known key with the default a missing one takes.
struct Spec {
    key: &'static str,
    kind: Kind,
    default: &'static str,
}

const fn spec(key: &'static str, kind: Kind, default: &'static str) -> Spec {
    Spec { key, kind, default }
}

/// `[app]` (DESIGN §6.2).
const APP_KEYS: &[Spec] = &[
    spec("scale", Kind::Int, "3"),
    spec("fullscreen", Kind::Bool, "false"),
    spec("volume", Kind::Int, "100"),
    spec("mute", Kind::Bool, "false"),
    spec("audio_rate", Kind::Int, "0"),
    spec("rewind_seconds", Kind::Int, "10"),
    spec("status_bar", Kind::Bool, "true"),
    spec("last_folder", Kind::Text, ""),
];

/// `[gb]`: gb `config.rs`'s keys, same names and defaults.
const GB_KEYS: &[Spec] = &[
    spec("integer_scale", Kind::Bool, "true"),
    spec("palette", Kind::Text, "dmg-green"),
    spec("filter", Kind::Text, "none"),
    spec("record", Kind::Text, "off"),
    spec("frameskip", Kind::Text, "off"),
    spec("link_role", Kind::Text, "off"),
    spec("link_port", Kind::Int, "8765"),
    spec("muted", Kind::Bool, "false"),
    spec("sample_rate", Kind::Text, "auto"),
    spec("channel_pulse1", Kind::Bool, "true"),
    spec("channel_pulse2", Kind::Bool, "true"),
    spec("channel_wave", Kind::Bool, "true"),
    spec("channel_noise", Kind::Bool, "true"),
    spec("ff_speed", Kind::Int, "4"),
    spec("boot_rom", Kind::Text, ""),
    spec(
        "boot_choice",
        Kind::Choice(&["off", "builtin", "file"]),
        "off",
    ),
    spec(
        "console",
        Kind::Choice(&["auto", "dmg", "cgb", "sgb"]),
        "auto",
    ),
    spec("sgb_rom", Kind::Text, ""),
    spec("keys_file", Kind::Text, ""),
    spec("gamepad", Kind::Bool, "true"),
    spec("state_slot", Kind::Int, "1"),
    spec("rewind", Kind::Choice(&["off", "30", "60", "120"]), "60"),
    spec("per_game", Kind::Bool, "false"),
    spec("key_right", Kind::Text, ""),
    spec("key_left", Kind::Text, ""),
    spec("key_up", Kind::Text, ""),
    spec("key_down", Kind::Text, ""),
    spec("key_a", Kind::Text, ""),
    spec("key_b", Kind::Text, ""),
    spec("key_select", Kind::Text, ""),
    spec("key_start", Kind::Text, ""),
];

/// `[gba]`: the GBA front end's BIOS key, plus the state slot the shared slot row writes.
const GBA_KEYS: &[Spec] = &[
    spec("bios", Kind::Text, ""),
    spec("state_slot", Kind::Int, "1"),
];

/// `[nes]`: nes `config.rs`'s keys, same names and defaults, plus the FDS BIOS #21 adds.
const NES_KEYS: &[Spec] = &[
    spec("palette", Kind::Text, ""),
    spec("keys_file", Kind::Text, ""),
    spec("start_fullscreen", Kind::Bool, "false"),
    spec(
        "ram_init",
        Kind::Choice(&["zero", "ones", "random"]),
        "zero",
    ),
    spec("align", Kind::Text, ""),
    spec("reset_align", Kind::Text, ""),
    spec("ntsc", Kind::Bool, "false"),
    spec("sync", Kind::Choice(&["audio", "video"]), "audio"),
    spec("crt", Kind::Text, "off"),
    spec("rewind_seconds", Kind::Int, "10"),
    spec("random_mapper", Kind::Bool, "false"),
    spec("overclock", Kind::Int, "0"),
    spec("sprite_limit", Kind::Bool, "true"),
    spec("run_ahead", Kind::Int, "0"),
    spec("four_score", Kind::Bool, "false"),
    spec("vs_dip", Kind::Int, "0"),
    spec("history", Kind::Bool, "false"),
    spec("seed", Kind::Int, "0"),
    spec("crop_top", Kind::Int, "0"),
    spec("crop_bottom", Kind::Int, "0"),
    spec("crop_side", Kind::Int, "0"),
    spec("aspect", Kind::Choice(&["square", "4:3"]), "square"),
    spec("integer_scale", Kind::Bool, "false"),
    spec("mute", Kind::Bool, "false"),
    spec("silence_ultrasonic_triangle", Kind::Bool, "false"),
    spec("reduce_dmc_pop", Kind::Bool, "false"),
    spec("quirks", Kind::Text, ""),
    spec("fds_bios", Kind::Text, ""),
];

/// `[snes]`: snes `config.rs`'s keys, same names and defaults.
const SNES_KEYS: &[Spec] = &[
    spec("integer_scale", Kind::Bool, "true"),
    spec("muted", Kind::Bool, "false"),
    spec("ff_speed", Kind::Int, "4"),
    spec("channel_voice1", Kind::Bool, "true"),
    spec("channel_voice2", Kind::Bool, "true"),
    spec("channel_voice3", Kind::Bool, "true"),
    spec("channel_voice4", Kind::Bool, "true"),
    spec("channel_voice5", Kind::Bool, "true"),
    spec("channel_voice6", Kind::Bool, "true"),
    spec("channel_voice7", Kind::Bool, "true"),
    spec("channel_voice8", Kind::Bool, "true"),
    spec("keys_file", Kind::Text, ""),
    spec("gamepad", Kind::Bool, "true"),
    spec("state_slot", Kind::Int, "1"),
    spec("key_right", Kind::Text, ""),
    spec("key_left", Kind::Text, ""),
    spec("key_up", Kind::Text, ""),
    spec("key_down", Kind::Text, ""),
    spec("key_a", Kind::Text, ""),
    spec("key_b", Kind::Text, ""),
    spec("key_x", Kind::Text, ""),
    spec("key_y", Kind::Text, ""),
    spec("key_l", Kind::Text, ""),
    spec("key_r", Kind::Text, ""),
    spec("key_select", Kind::Text, ""),
    spec("key_start", Kind::Text, ""),
];

/// The pass-through families of the nes mixer and CRT settings: numbers, names or pairs this layer
/// does not interpret, kept as written for the nes rows (#26).
const NES_RAW_PREFIXES: [&str; 3] = ["gain_", "pan_", "eq_"];
const NES_CRT_KEYS: [&str; 6] = [
    "crt_scanline_strength",
    "crt_mask",
    "crt_mask_strength",
    "crt_curvature",
    "crt_bloom",
    "crt_enabled",
];

const RAW: Spec = spec("", Kind::Text, "");

/// The system's own key table.
fn system_keys(section: &str) -> &'static [Spec] {
    match section {
        "gb" => GB_KEYS,
        "gba" => GBA_KEYS,
        "nes" => NES_KEYS,
        "snes" => SNES_KEYS,
        _ => &[],
    }
}

fn find(specs: &'static [Spec], key: &str) -> Option<&'static Spec> {
    specs.iter().find(|spec| spec.key == key)
}

/// The one spelling this build stores a key under: the source's own name wins (`sgb_rom`, not
/// `sgb_sound_rom`; gb and snes say `muted`, nes says `mute`), with the aliases accepted.
fn canonical<'a>(section: &str, key: &'a str) -> &'a str {
    match key {
        "sgb_sound_rom" if section == "gb" => "sgb_rom",
        "muted" if section == APP => "mute",
        "mute" if section == "gb" || section == "snes" => "muted",
        _ => key,
    }
}

/// The spec for `key` in `section`, if the section knows it.
fn spec_for(section: &str, key: &str) -> Option<&'static Spec> {
    let key = canonical(section, key);
    if section == APP {
        return find(APP_KEYS, key);
    }
    find(system_keys(section), key)
        .or_else(|| find(APP_KEYS, key))
        .or_else(|| {
            let raw = NES_RAW_PREFIXES
                .iter()
                .any(|prefix| key.starts_with(prefix))
                || (section == "nes" && NES_CRT_KEYS.contains(&key));
            raw.then_some(&RAW)
        })
}

/// One settings file in memory: `section -> key -> value`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Config {
    sections: BTreeMap<String, BTreeMap<String, String>>,
}

impl Config {
    /// An empty file: every key takes its default.
    pub fn new() -> Config {
        Config::default()
    }

    /// The stored value, without defaults, for an exact section and key.
    pub fn get(&self, section: &str, key: &str) -> Option<&str> {
        self.sections
            .get(section)?
            .get(canonical(section, key))
            .map(String::as_str)
    }

    /// Stores `value` in `section`.
    pub fn set(&mut self, section: &str, key: &str, value: impl Into<String>) {
        self.sections
            .entry(section.to_string())
            .or_default()
            .insert(canonical(section, key).to_string(), value.into());
    }

    /// Drops a stored value.
    pub fn remove(&mut self, section: &str, key: &str) -> Option<String> {
        let key = canonical(section, key);
        let value = self.sections.get_mut(section)?.remove(key);
        if self.sections.get(section).is_some_and(BTreeMap::is_empty) {
            self.sections.remove(section);
        }
        value
    }

    /// Whether the section holds no stored key.
    pub fn is_empty_section(&self, section: &str) -> bool {
        self.sections.get(section).is_none_or(BTreeMap::is_empty)
    }

    /// The value `system` runs with: its section, then `[app]`, then the built-in default.
    pub fn value(&self, system: SystemKind, key: &str) -> Option<&str> {
        let section = system.key();
        if let Some(value) = self.get(section, key) {
            return Some(value);
        }
        if let Some(value) = self.get(APP, key) {
            return Some(value);
        }
        spec_for(section, key).map(|spec| spec.default)
    }

    /// All keys of one section, in schema order then alphabetical.
    fn ordered(&self, section: &str) -> Vec<(&str, &str)> {
        let Some(values) = self.sections.get(section) else {
            return Vec::new();
        };
        let mut keys: Vec<&str> = Vec::with_capacity(values.len());
        for spec in system_keys(section) {
            if values.contains_key(spec.key) {
                keys.push(spec.key);
            }
        }
        for spec in APP_KEYS {
            if section != APP && values.contains_key(spec.key) && !keys.contains(&spec.key) {
                keys.push(spec.key);
            }
        }
        let mut extra: Vec<&str> = values
            .keys()
            .map(String::as_str)
            .filter(|key| !keys.contains(key))
            .collect();
        extra.sort_unstable();
        keys.extend(extra);
        keys.into_iter()
            .map(|key| (key, values[key].as_str()))
            .collect()
    }
}

/// A parsed file plus the lines the reader should mention.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Loaded {
    pub config: Config,
    pub notices: Vec<String>,
}

/// Parse `config.toml` text. A missing key takes its default; an unknown key is a notice and is
/// dropped; a bad value is an error naming the file and its 1-based line.
pub fn parse(text: &str) -> Result<Loaded, String> {
    parse_named(text, APP, "config.toml")
}

/// Parse `text` as if its top-level keys were under `default_section`, with `file` in messages.
pub fn parse_named(text: &str, default_section: &str, file: &str) -> Result<Loaded, String> {
    let mut config = Config::new();
    let mut notices = Vec::new();
    let mut section = default_section.to_string();
    let mut unknown_section = false;
    for (offset, raw) in text.lines().enumerate() {
        let line_number = offset + 1;
        let line = strip_comment(raw).trim();
        if line.is_empty() {
            continue;
        }
        if let Some(name) = line
            .strip_prefix('[')
            .and_then(|rest| rest.strip_suffix(']'))
        {
            let name = name.trim().to_ascii_lowercase();
            if SECTIONS.contains(&name.as_str()) {
                section = name;
                unknown_section = false;
            } else {
                notices.push(format!(
                    "{file}: line {line_number}: unknown section `[{name}]`"
                ));
                unknown_section = true;
            }
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            return Err(format!(
                "{file}: line {line_number}: expected `key = value`, got {line:?}"
            ));
        };
        let key = key.trim();
        let value = value.trim();
        if unknown_section {
            continue;
        }
        let Some(spec) = spec_for(&section, key) else {
            notices.push(format!("{file}: line {line_number}: unknown key `{key}`"));
            continue;
        };
        let value = match spec.kind {
            Kind::Bool => match value {
                "true" => "true".to_string(),
                "false" => "false".to_string(),
                _ => {
                    return Err(format!(
                        "{file}: line {line_number}: {} must be true or false, got {value:?}",
                        canonical(&section, key)
                    ));
                }
            },
            Kind::Int => match value.parse::<i64>() {
                Ok(number) => number.to_string(),
                Err(_) => {
                    return Err(format!(
                        "{file}: line {line_number}: {} must be a whole number, got {value:?}",
                        canonical(&section, key)
                    ));
                }
            },
            Kind::Text => unquote(value).to_string(),
            Kind::Choice(words) => {
                let word = unquote(value);
                if words.contains(&word) {
                    word.to_string()
                } else {
                    spec.default.to_string()
                }
            }
        };
        config.set(&section, key, value);
    }
    Ok(Loaded { config, notices })
}

fn unquote(value: &str) -> &str {
    value
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
        .unwrap_or(value)
}

/// Drop a trailing `#` comment, ignoring `#` inside a double-quoted value.
fn strip_comment(line: &str) -> &str {
    let mut in_quotes = false;
    for (offset, ch) in line.char_indices() {
        match ch {
            '"' => in_quotes = !in_quotes,
            '#' if !in_quotes => return &line[..offset],
            _ => {}
        }
    }
    line
}

/// One key's line, quoted where the kind is not a bare boolean or number.
fn line(section: &str, key: &str, value: &str) -> String {
    let bare =
        spec_for(section, key).is_some_and(|spec| matches!(spec.kind, Kind::Bool | Kind::Int));
    if bare {
        format!("{key} = {value}\n")
    } else {
        format!("{key} = \"{value}\"\n")
    }
}

/// The file form: sections in a fixed order, keys in schema order, and every stored key written.
pub fn to_text(config: &Config) -> String {
    let mut text = String::new();
    for section in SECTIONS {
        let keys = config.ordered(section);
        if keys.is_empty() {
            continue;
        }
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(&format!("[{section}]\n"));
        for (key, value) in keys {
            text.push_str(&line(section, key, value));
        }
    }
    text
}

/// The file form of a per-game overlay: the same keys, no section header.
pub fn to_flat_text(config: &Config, section: &str) -> String {
    let mut text = String::new();
    for (key, value) in config.ordered(section) {
        text.push_str(&line(section, key, value));
    }
    text
}

/// A file's identity for the cache: its modification time (or none) and its length.
type Stamp = (Option<SystemTime>, u64);

fn stamp(path: &Path) -> Stamp {
    match std::fs::metadata(path) {
        Ok(meta) => (meta.modified().ok(), meta.len()),
        Err(_) => (None, 0),
    }
}

/// One parsed file, keyed by root and file stamp, so the drawer's many row reads do not re-read the
/// disk every frame.
struct Cached {
    root: PathBuf,
    stamp: Stamp,
    loaded: Loaded,
}

static CACHE: Mutex<Option<Cached>> = Mutex::new(None);

/// The settings file, read through the cache. A missing file is the defaults; an unreadable or
/// malformed one is the defaults plus one notice.
pub fn load(paths: &Paths) -> Loaded {
    let file = paths.config_file();
    let stamp = stamp(&file);
    {
        let cache = CACHE.lock().unwrap_or_else(|poison| poison.into_inner());
        if let Some(cached) = cache.as_ref()
            && cached.root == paths.root()
            && cached.stamp == stamp
        {
            return cached.loaded.clone();
        }
    }
    let loaded = match std::fs::read_to_string(&file) {
        Ok(text) => match parse_named(&text, APP, "config.toml") {
            Ok(loaded) => loaded,
            Err(why) => Loaded {
                notices: vec![why],
                ..Loaded::default()
            },
        },
        Err(why) if why.kind() == io::ErrorKind::NotFound => Loaded::default(),
        Err(why) => Loaded {
            notices: vec![format!("config.toml not read: {why}")],
            ..Loaded::default()
        },
    };
    let mut cache = CACHE.lock().unwrap_or_else(|poison| poison.into_inner());
    *cache = Some(Cached {
        root: paths.root().to_path_buf(),
        stamp,
        loaded: loaded.clone(),
    });
    loaded
}

/// Writes the settings file: a temporary file, then a rename (§6.2).
pub fn save(paths: &Paths, config: &Config) -> io::Result<()> {
    let file = paths.config_file();
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let result = write_atomic(&file, to_text(config).as_bytes());
    if result.is_ok() {
        // Our own write must be visible at once, even when the clock's file stamp does not move.
        let mut cache = CACHE.lock().unwrap_or_else(|poison| poison.into_inner());
        if cache
            .as_ref()
            .is_some_and(|cached| cached.root == paths.root())
        {
            *cache = None;
        }
    }
    result
}

/// Stores one value and writes the file.
pub fn write_value(paths: &Paths, section: &str, key: &str, value: &str) -> io::Result<()> {
    let mut config = load(paths).config;
    config.set(section, key, value);
    save(paths, &config)
}

/// The value `system` runs with, per-game file first: what `App::setting` asks for.
pub fn setting(app: &App, system: SystemKind, key: &str) -> Option<String> {
    if app.system() == Some(system)
        && let Some(machine) = app.machine()
        && let Some(value) =
            crate::games::value(app.paths(), system, machine.cart_identity().crc32, key)
    {
        return Some(value);
    }
    load(app.paths())
        .config
        .value(system, key)
        .map(str::to_string)
}

/// The drawer's `CONFIG FILE` rows: where the file is, and the first line it had to ignore.
pub fn rows(app: &App) -> Vec<Row> {
    let loaded = load(app.paths());
    let mut rows = vec![Row::info(
        "File",
        app.paths().config_file().display().to_string(),
        "Where the settings are written",
    )];
    if let Some(notice) = loaded.notices.first() {
        rows.push(Row::info(
            "Ignored",
            notice.clone(),
            "A line this build does not know",
        ));
    }
    rows
}

/// No config row changes anything.
pub fn on_row(_app: &mut App, _index: usize, _act: RowAct) -> bool {
    false
}

static SHOWN: Mutex<Option<(PathBuf, Stamp)>> = Mutex::new(None);

/// Shows the first ignored line when the file is new or changed, so a typo reaches the bar exactly
/// once per edit.
pub fn on_frame(app: &mut App) {
    let file = app.paths().config_file();
    let stamp = stamp(&file);
    let changed = {
        let mut shown = SHOWN.lock().unwrap_or_else(|poison| poison.into_inner());
        let changed = shown
            .as_ref()
            .is_none_or(|(root, seen)| *root != app.paths().root() || *seen != stamp);
        if changed {
            *shown = Some((app.paths().root().to_path_buf(), stamp));
        }
        changed
    };
    if !changed {
        return;
    }
    if let Some(notice) = load(app.paths()).notices.first() {
        app.show_notice(notice.clone(), false);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_text_is_the_defaults() {
        let loaded = parse("").unwrap();
        assert_eq!(loaded.config, Config::new());
        assert!(loaded.notices.is_empty());
        assert_eq!(loaded.config.value(SystemKind::Gb, "scale"), Some("3"));
    }

    #[test]
    fn comments_and_blank_lines_are_ignored() {
        let text = "[app]\n# a comment\n\nscale = 5 # the scale\nfullscreen = true\n";
        let loaded = parse(text).unwrap();
        assert_eq!(loaded.config.value(SystemKind::Snes, "scale"), Some("5"));
        assert_eq!(
            loaded.config.value(SystemKind::Snes, "fullscreen"),
            Some("true")
        );
    }

    #[test]
    fn a_bad_value_names_the_line() {
        let why = parse("[app]\nscale = 4\nvolume = 101x\n").unwrap_err();
        assert!(why.contains("line 3"), "{why}");
        assert!(why.starts_with("config.toml:"), "{why}");
    }

    #[test]
    fn an_unknown_key_is_a_notice_and_keeps_the_defaults() {
        let loaded = parse("nonsense = 1\n[app]\nscale = 3\n").unwrap();
        assert_eq!(loaded.config.value(SystemKind::Gb, "scale"), Some("3"));
        assert_eq!(loaded.notices.len(), 1);
        assert!(loaded.notices[0].contains("unknown key `nonsense`"));
    }

    #[test]
    fn a_round_trip_is_equal() {
        let mut config = Config::new();
        config.set("gb", "scale", "6");
        config.set("gb", "palette", "pocket");
        config.set("app", "volume", "40");
        config.set("gba", "bios", "bios/gba.bin");
        config.set("nes", "fds_bios", "disks/fds.bin");
        config.set("gb", "channel_pulse1", "false");
        let text = to_text(&config);
        assert_eq!(parse(&text).unwrap().config, config);
    }

    #[test]
    fn the_console_round_trips_and_an_unknown_word_is_auto() {
        for word in ["auto", "dmg", "cgb", "sgb"] {
            let loaded = parse(&format!("[gb]\nconsole = \"{word}\"\n")).unwrap();
            assert_eq!(
                loaded.config.value(SystemKind::Gb, "console"),
                Some(word),
                "{word}"
            );
        }
        let loaded = parse("[gb]\nconsole = \"supergb\"\n").unwrap();
        assert_eq!(loaded.config.value(SystemKind::Gb, "console"), Some("auto"));
        assert!(!to_text(&Config::new()).contains("console"));
    }

    #[test]
    fn the_source_alias_sgb_sound_rom_is_the_sgb_rom() {
        let loaded = parse("[gb]\nsgb_sound_rom = \"sgb.bin\"\n").unwrap();
        assert_eq!(
            loaded.config.value(SystemKind::Gb, "sgb_rom"),
            Some("sgb.bin")
        );
    }

    #[test]
    fn a_write_replaces_the_file_atomically_and_leaves_no_temporary() {
        let root = std::env::temp_dir().join(format!("tendyemu-config-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let paths = Paths::new(&root);
        write_value(&paths, "gb", "scale", "5").unwrap();
        assert_eq!(
            load(&paths).config.value(SystemKind::Gb, "scale"),
            Some("5")
        );
        assert!(!paths.config_file().with_extension("tmp").exists());
        let _ = std::fs::remove_dir_all(&root);
    }
}
