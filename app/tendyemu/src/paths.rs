//! Where TendyEmu keeps its files (DESIGN §6.3).
//!
//! One function per path, all under a [`Paths`] root so a test can point them at a scratch folder.
//! `default_root` is the rule the GB and SNES front ends already use, and [`Paths::ensure_layout`]
//! creates the fresh-install folders — the same shape gb and snes already use.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

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

    /// Creates the folder layout of a fresh install (§6.3): every system's per-ROM folders, so the
    /// first save, state, screenshot or recording already has a home. Existing folders are left
    /// alone.
    pub fn ensure_layout(&self) -> io::Result<()> {
        std::fs::create_dir_all(&self.root)?;
        for system in SystemKind::ALL {
            std::fs::create_dir_all(self.system_dir(system))?;
            std::fs::create_dir_all(self.games_dir(system))?;
            std::fs::create_dir_all(self.saves_dir(system))?;
            std::fs::create_dir_all(self.states_dir(system))?;
            std::fs::create_dir_all(self.screenshots_dir(system))?;
            std::fs::create_dir_all(self.recordings_dir(system))?;
        }
        // Only the two systems with cheats, and only the nes with movies (§6.3).
        std::fs::create_dir_all(self.cheats_dir(SystemKind::Gb))?;
        std::fs::create_dir_all(self.cheats_dir(SystemKind::Nes))?;
        std::fs::create_dir_all(self.movies_dir(SystemKind::Nes))?;
        Ok(())
    }
}

/// The drawer's `FOLDERS` rows: where everything is written for the running system.
pub fn rows(app: &App) -> Vec<Row> {
    let paths = app.paths();
    let mut rows = vec![Row::info(
        "Root",
        paths.root().display().to_string(),
        "Every folder below hangs off this one",
    )];
    if let Some(system) = app.system() {
        for (label, path) in [
            ("Saves", paths.saves_dir(system)),
            ("States", paths.states_dir(system)),
            ("Screenshots", paths.screenshots_dir(system)),
            ("Recordings", paths.recordings_dir(system)),
        ] {
            rows.push(Row::info(
                label,
                path.strip_prefix(paths.root()).map_or_else(
                    |_| path.display().to_string(),
                    |rest| rest.display().to_string(),
                ),
                "A folder of this system's own",
            ));
        }
    }
    rows
}

/// No folder row does anything.
pub fn on_row(_app: &mut App, _index: usize, _act: RowAct) -> bool {
    false
}

static READY: Mutex<Option<PathBuf>> = Mutex::new(None);

/// Creates the layout once per root. A failure is shown once; the next frame does not retry a
/// folder that cannot be made.
pub fn on_frame(app: &mut App) {
    let root = app.paths().root().to_path_buf();
    {
        let ready = READY.lock().unwrap_or_else(|poison| poison.into_inner());
        if ready.as_ref() == Some(&root) {
            return;
        }
    }
    match app.paths().ensure_layout() {
        Ok(()) => {}
        Err(why) => app.show_notice(format!("folders not created: {why}"), true),
    }
    let mut ready = READY.lock().unwrap_or_else(|poison| poison.into_inner());
    *ready = Some(root);
}

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

    #[test]
    fn a_fresh_install_gets_every_folder_and_no_config_file() {
        let root = std::env::temp_dir().join(format!("tendyemu-paths-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let paths = Paths::new(&root);
        paths.ensure_layout().unwrap();
        for system in SystemKind::ALL {
            for dir in [
                paths.saves_dir(system),
                paths.states_dir(system),
                paths.screenshots_dir(system),
                paths.recordings_dir(system),
                paths.games_dir(system),
            ] {
                assert!(dir.is_dir(), "{}", dir.display());
            }
        }
        assert!(paths.cheats_dir(SystemKind::Gb).is_dir());
        assert!(paths.cheats_dir(SystemKind::Nes).is_dir());
        assert!(paths.movies_dir(SystemKind::Nes).is_dir());
        assert!(!paths.config_file().exists());
        let _ = std::fs::remove_dir_all(&root);
    }
}
