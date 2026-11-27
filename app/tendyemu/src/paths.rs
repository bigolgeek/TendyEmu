//! Where TendyEmu keeps its files (DESIGN §6.3).
//!
//! One function per path, all under a [`Paths`] root so a test can point them at a scratch folder.
//! `default_root` is the rule the GB and SNES front ends already use; #21 replaces the body with
//! the folder creation of a fresh install, without changing a signature here.

use std::path::{Path, PathBuf};

use tendy_core::SystemKind;

use crate::app::App;
use crate::menu::{Row, RowAct};

/// The default root: `%APPDATA%\tendyemu` on Windows, `$XDG_CONFIG_HOME/tendyemu` else
/// `~/.config/tendyemu` on Linux (§6.3).
pub fn default_root() -> PathBuf {
    if cfg!(windows) {
        if let Some(appdata) = std::env::var_os("APPDATA") {
            return PathBuf::from(appdata).join("tendyemu");
        }
    } else if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
        return PathBuf::from(xdg).join("tendyemu");
    } else if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home).join(".config").join("tendyemu");
    }
    PathBuf::from("tendyemu")
}

/// The root every other path hangs off.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Paths {
    root: PathBuf,
}

impl Default for Paths {
    fn default() -> Paths {
        Paths::new(default_root())
    }
}

impl Paths {
    pub fn new(root: impl Into<PathBuf>) -> Paths {
        Paths { root: root.into() }
    }

    /// The root folder itself.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// `config.toml` (§6.2).
    pub fn config_file(&self) -> PathBuf {
        self.root.join("config.toml")
    }

    /// `recents.txt` (§6.3: one `<system key> TAB <file>` per line).
    pub fn recents_file(&self) -> PathBuf {
        self.root.join("recents.txt")
    }

    /// `first-run.done`, written once the first-run offer is answered.
    pub fn first_run_done(&self) -> PathBuf {
        self.root.join("first-run.done")
    }

    /// `<root>/<system key>`, the folder every per-ROM file of a system hangs off.
    pub fn system_dir(&self, system: SystemKind) -> PathBuf {
        self.root.join(system.key())
    }

    /// `<system>/keys.txt`.
    pub fn keys_file(&self, system: SystemKind) -> PathBuf {
        self.system_dir(system).join("keys.txt")
    }

    /// `<system>/games/<crc32>.toml`.
    pub fn games_dir(&self, system: SystemKind) -> PathBuf {
        self.system_dir(system).join("games")
    }

    /// `<system>/saves/`.
    pub fn saves_dir(&self, system: SystemKind) -> PathBuf {
        self.system_dir(system).join("saves")
    }

    /// `<system>/states/`.
    pub fn states_dir(&self, system: SystemKind) -> PathBuf {
        self.system_dir(system).join("states")
    }

    /// `<system>/screenshots/`.
    pub fn screenshots_dir(&self, system: SystemKind) -> PathBuf {
        self.system_dir(system).join("screenshots")
    }

    /// `<system>/recordings/`.
    pub fn recordings_dir(&self, system: SystemKind) -> PathBuf {
        self.system_dir(system).join("recordings")
    }

    /// `<system>/cheats/`.
    pub fn cheats_dir(&self, system: SystemKind) -> PathBuf {
        self.system_dir(system).join("cheats")
    }

    /// `<system>/movies/`.
    pub fn movies_dir(&self, system: SystemKind) -> PathBuf {
        self.system_dir(system).join("movies")
    }
}

/// No rows until the folder settings arrive (#21).
pub fn rows(_app: &App) -> Vec<Row> {
    Vec::new()
}

/// No row does anything yet.
pub fn on_row(_app: &mut App, _index: usize, _act: RowAct) -> bool {
    false
}

/// Nothing to do per frame yet.
pub fn on_frame(_app: &mut App) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_folder_hangs_off_the_root_and_the_system_key() {
        let paths = Paths::new("scratch");
        assert_eq!(
            paths.recents_file(),
            PathBuf::from("scratch").join("recents.txt")
        );
        assert_eq!(
            paths.saves_dir(SystemKind::Snes),
            PathBuf::from("scratch").join("snes").join("saves")
        );
        assert_eq!(
            paths.keys_file(SystemKind::Gb),
            PathBuf::from("scratch").join("gb").join("keys.txt")
        );
    }

    #[test]
    fn the_default_root_is_a_tendyemu_folder() {
        let root = default_root();
        assert_eq!(root.file_name().unwrap(), "tendyemu");
    }
}
