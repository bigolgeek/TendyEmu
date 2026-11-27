//! The `rfd` dialogs: ROM, boot ROM, keys file (DESIGN §6.1).
//!
//! The dialog only opens once the window loop has called [`set_interactive`], so a library caller
//! and a `cargo test` run never open one. The ROM filter is every extension the detector knows,
//! plus the two archive formats.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

/// The ROM filter: every extension `tendy_detect` names, and the two archive formats.
pub const ROM_FILTERS: [(&str, &[&str]); 3] = [
    (
        "ROM image",
        &[
            "gb", "gbc", "sgb", "gba", "agb", "nes", "unf", "unif", "fds", "qd", "nsf", "sfc",
            "smc", "swc", "fig",
        ],
    ),
    ("Zip archive", &["zip"]),
    ("7z archive", &["7z"]),
];

/// The boot ROM filter: one 256-byte file.
pub const BOOT_ROM_FILTER: (&str, &[&str]) = ("Boot ROM", &["bin"]);

/// The keys file filter.
pub const KEYS_FILTER: (&str, &[&str]) = ("Keys file", &["txt", "toml"]);

/// Whether a dialog may open. The window loop sets this at startup.
static INTERACTIVE: AtomicBool = AtomicBool::new(false);

/// Allows (or forbids) the dialogs. Forbidden until the windowed loop is running.
pub fn set_interactive(on: bool) {
    INTERACTIVE.store(on, Ordering::Relaxed);
}

/// Whether the dialogs may open right now.
pub fn is_interactive() -> bool {
    INTERACTIVE.load(Ordering::Relaxed)
}

/// One dialog with `filters` applied, starting in `start_dir` when it is a directory.
fn dialog(start_dir: &Path, filters: &[(&str, &[&str])]) -> Option<PathBuf> {
    if !is_interactive() {
        return None;
    }
    let mut picker = rfd::FileDialog::new();
    for (name, extensions) in filters {
        picker = picker.add_filter(*name, extensions);
    }
    if start_dir.is_dir() {
        picker = picker.set_directory(start_dir);
    }
    picker.pick_file()
}

/// The ROM dialog, starting in `start_dir`.
pub fn rom(start_dir: &Path) -> Option<PathBuf> {
    dialog(start_dir, &ROM_FILTERS)
}

/// The boot ROM dialog: one 256-byte `.bin`.
pub fn boot_rom(start_dir: &Path) -> Option<PathBuf> {
    dialog(start_dir, std::slice::from_ref(&BOOT_ROM_FILTER))
}

/// The keys file dialog.
pub fn keys(start_dir: &Path) -> Option<PathBuf> {
    dialog(start_dir, std::slice::from_ref(&KEYS_FILTER))
}

/// The folder a dialog should start in: the running ROM's own, or the working directory.
pub fn start_dir(rom: Option<&Path>) -> PathBuf {
    rom.and_then(Path::parent)
        .map(Path::to_path_buf)
        .filter(|dir| dir.is_dir())
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_dialog_never_opens_unless_the_window_loop_asked() {
        set_interactive(false);
        assert!(rom(Path::new(".")).is_none());
        assert!(boot_rom(Path::new(".")).is_none());
        assert!(keys(Path::new(".")).is_none());
    }

    #[test]
    fn the_filters_cover_every_extension_the_detector_knows() {
        let roms = ROM_FILTERS[0].1;
        for ext in [
            "gb", "gbc", "sgb", "gba", "agb", "nes", "unf", "unif", "fds", "qd", "nsf", "sfc",
            "smc", "swc", "fig",
        ] {
            assert!(roms.contains(&ext), "the ROM filter is missing {ext}");
        }
        assert_eq!(ROM_FILTERS[1].1, &["zip"]);
        assert_eq!(ROM_FILTERS[2].1, &["7z"]);
    }

    #[test]
    fn the_start_folder_is_the_rom_s_own() {
        let dir = std::env::current_dir().unwrap();
        assert_eq!(start_dir(Some(&dir.join("x.gb"))), dir);
    }
}
