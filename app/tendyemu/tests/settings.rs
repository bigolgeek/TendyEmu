//! Settings, per-game settings and folders per system (DESIGN §6.2, §6.3, SPEC §7; issue #21).
//!
//! Every test points a [`Paths`] at a throwaway folder. The ROM image is built in code, so no test
//! needs a personal library.

use std::fs;
use std::path::PathBuf;

use tendy_core::SystemKind;
use tendyemu::app::App;
use tendyemu::config;
use tendyemu::games;
use tendyemu::paths::{self, Paths};

/// A scratch config folder, removed when the test ends.
struct Scratch {
    root: PathBuf,
}

impl Scratch {
    fn new(name: &str) -> Scratch {
        let root =
            std::env::temp_dir().join(format!("tendyemu-settings-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("the scratch folder is writable");
        Scratch { root }
    }

    fn paths(&self) -> Paths {
        Paths::new(self.root.clone())
    }

    /// Writes `bytes` as `name` inside the scratch folder and hands back the path.
    fn rom(&self, name: &str, bytes: &[u8]) -> PathBuf {
        let path = self.root.join(name);
        fs::write(&path, bytes).expect("the scratch ROM is writable");
        path
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// A 32 KiB Game Boy cartridge: a title, a valid header checksum and no mapper.
fn gb_rom() -> Vec<u8> {
    let mut rom = vec![0u8; 0x8000];
    rom[0x100..0x104].copy_from_slice(&[0x00, 0xC3, 0x50, 0x01]);
    rom[0x134..0x13C].copy_from_slice(b"SETTINGS");
    rom[0x147] = 0x00;
    rom[0x148] = 0x00;
    rom[0x149] = 0x00;
    let mut sum = 0u8;
    for byte in &rom[0x134..=0x14C] {
        sum = sum.wrapping_sub(*byte).wrapping_sub(1);
    }
    rom[0x14D] = sum;
    rom
}

#[test]
fn system_section_never_changes_another() {
    let scratch = Scratch::new("isolation");
    let paths = scratch.paths();
    config::write_value(&paths, "gb", "scale", "6").unwrap();
    config::write_value(&paths, "snes", "volume", "40").unwrap();

    let loaded = config::load(&paths);
    assert_eq!(loaded.config.value(SystemKind::Gb, "scale"), Some("6"));
    assert_eq!(loaded.config.value(SystemKind::Snes, "scale"), Some("3"));
    assert_eq!(loaded.config.value(SystemKind::Gb, "volume"), Some("100"));
    assert_eq!(loaded.config.value(SystemKind::Snes, "volume"), Some("40"));

    let text = fs::read_to_string(paths.config_file()).unwrap();
    assert!(text.contains("[gb]"), "{text}");
    assert!(text.contains("[snes]"), "{text}");
}

#[test]
fn fresh_install_creates_folders() {
    let scratch = Scratch::new("fresh");
    let paths = scratch.paths();
    let mut app = App::new(paths.clone());
    paths::on_frame(&mut app);

    for system in SystemKind::ALL {
        for dir in [
            paths.system_dir(system),
            paths.games_dir(system),
            paths.saves_dir(system),
            paths.states_dir(system),
            paths.screenshots_dir(system),
            paths.recordings_dir(system),
        ] {
            assert!(dir.is_dir(), "{}", dir.display());
        }
    }
    assert!(paths.cheats_dir(SystemKind::Gb).is_dir());
    assert!(paths.cheats_dir(SystemKind::Nes).is_dir());
    assert!(paths.movies_dir(SystemKind::Nes).is_dir());
    // The layout, not a file: settings are written on the first change.
    assert!(!paths.config_file().exists());
}

#[test]
fn missing_key_is_default() {
    let loaded =
        config::parse("[gb]\nscale = 5\n[snes]\nvolume = 40\n[app]\nrewind_seconds = 30\n")
            .unwrap();
    assert!(loaded.notices.is_empty(), "{:?}", loaded.notices);
    assert_eq!(loaded.config.value(SystemKind::Gb, "scale"), Some("5"));
    assert_eq!(loaded.config.value(SystemKind::Gb, "console"), Some("auto"));
    assert_eq!(loaded.config.value(SystemKind::Gb, "muted"), Some("false"));
    assert_eq!(loaded.config.value(SystemKind::Snes, "volume"), Some("40"));
    assert_eq!(loaded.config.value(SystemKind::Gb, "volume"), Some("100"));
    assert_eq!(
        loaded.config.value(SystemKind::Gb, "rewind_seconds"),
        Some("30")
    );
    assert_eq!(loaded.config.value(SystemKind::Nes, "sync"), Some("audio"));
}

#[test]
fn unknown_key_gives_one_notice() {
    let loaded = config::parse("[app]\nscale = 3\nnot_a_key = 7\n").unwrap();
    assert_eq!(loaded.notices.len(), 1, "{:?}", loaded.notices);
    assert!(loaded.notices[0].contains("unknown key `not_a_key`"));
    assert!(loaded.config.get("app", "not_a_key").is_none());
    assert_eq!(loaded.config.value(SystemKind::Gb, "scale"), Some("3"));
}

#[test]
fn system_section_overrides_app_key() {
    let scratch = Scratch::new("override");
    let paths = scratch.paths();
    config::write_value(&paths, "app", "scale", "4").unwrap();
    config::write_value(&paths, "gb", "scale", "6").unwrap();

    let loaded = config::load(&paths);
    assert_eq!(loaded.config.value(SystemKind::Gb, "scale"), Some("6"));
    assert_eq!(loaded.config.value(SystemKind::Snes, "scale"), Some("4"));

    // Dropping the system's own value leaves the shared one in force.
    let mut config = loaded.config;
    config.remove("gb", "scale");
    config::save(&paths, &config).unwrap();
    assert_eq!(
        config::load(&paths).config.value(SystemKind::Gb, "scale"),
        Some("4")
    );
}

#[test]
fn per_game_overrides_system_section() {
    let scratch = Scratch::new("per-game");
    let paths = scratch.paths();
    config::write_value(&paths, "gb", "scale", "4").unwrap();
    let rom = scratch.rom("game.gb", &gb_rom());
    let mut app = App::new(paths.clone());
    app.open(&rom).expect("the built cartridge opens");
    let crc32 = app.machine().expect("a machine runs").cart_identity().crc32;

    assert_eq!(
        config::setting(&app, SystemKind::Gb, "scale").as_deref(),
        Some("4")
    );
    games::set(&paths, SystemKind::Gb, crc32, "scale", "6").unwrap();
    assert!(games::path(&paths, SystemKind::Gb, crc32).is_file());
    assert_eq!(
        config::setting(&app, SystemKind::Gb, "scale").as_deref(),
        Some("6")
    );
    // Another system keeps its own value while a game of this one runs.
    assert_eq!(
        config::setting(&app, SystemKind::Snes, "scale").as_deref(),
        Some("3")
    );

    games::clear(&paths, SystemKind::Gb, crc32).unwrap();
    assert!(!games::path(&paths, SystemKind::Gb, crc32).exists());
    assert_eq!(
        config::setting(&app, SystemKind::Gb, "scale").as_deref(),
        Some("4")
    );
}
