//! The last ten ROMs opened, one per line with the system it ran as (DESIGN §6.3, §7.6).
//!
//! The file is `<system key> TAB <file>`, newest first, so a line says which core to build before
//! the file is even read. No serde: a plain text file, a missing one read as an empty list, and
//! every rewrite through the atomic temp-file rename of [`crate::battery::write_atomic`].

use std::io;
use std::path::{Path, PathBuf};

use tendy_core::SystemKind;

use crate::battery::write_atomic;
use crate::paths::Paths;

/// How many ROM paths are kept.
pub const MAX_RECENTS: usize = 10;

/// One remembered ROM.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Recent {
    /// The system it was opened as.
    pub system: SystemKind,
    /// The file itself.
    pub path: PathBuf,
    /// The file is no longer there: the drawer greys the row out.
    pub gone: bool,
}

/// The system a stored key names, case-insensitively.
pub fn system_from_key(key: &str) -> Option<SystemKind> {
    SystemKind::ALL
        .into_iter()
        .find(|system| system.key().eq_ignore_ascii_case(key.trim()))
}

/// The remembered ROMs, most recently used first. A missing file is an empty list.
pub fn load(paths: &Paths) -> Vec<Recent> {
    let Ok(text) = std::fs::read_to_string(paths.recents_file()) else {
        return Vec::new();
    };
    let mut recents: Vec<Recent> = text
        .lines()
        .filter_map(|line| {
            let line = line.trim_end_matches('\r');
            let (key, file) = line.split_once('\t')?;
            let file = file.trim();
            if file.is_empty() {
                return None;
            }
            let system = system_from_key(key)?;
            let path = PathBuf::from(file);
            let gone = !path.is_file();
            Some(Recent { system, path, gone })
        })
        .collect();
    recents.truncate(MAX_RECENTS);
    recents
}

/// Rewrites the file from a newest-first list.
fn write(paths: &Paths, recents: &[Recent]) -> io::Result<()> {
    let mut text = String::new();
    for recent in recents {
        text.push_str(recent.system.key());
        text.push('\t');
        text.push_str(&recent.path.to_string_lossy());
        text.push('\n');
    }
    if let Some(dir) = paths.recents_file().parent() {
        std::fs::create_dir_all(dir)?;
    }
    write_atomic(&paths.recents_file(), text.as_bytes())
}

/// Remembers `path` as the most recent ROM: it moves to the front, the list is capped at
/// [`MAX_RECENTS`], and the file is rewritten.
pub fn record(paths: &Paths, system: SystemKind, path: &Path) -> io::Result<Vec<Recent>> {
    let mut recents = load(paths);
    recents.retain(|recent| recent.path != path);
    recents.insert(
        0,
        Recent {
            system,
            path: path.to_path_buf(),
            gone: false,
        },
    );
    recents.truncate(MAX_RECENTS);
    write(paths, &recents)?;
    Ok(recents)
}

/// Drops `gone` from the list. A path that is not in the list leaves the file untouched.
pub fn forget(paths: &Paths, gone: &Path) -> io::Result<Vec<Recent>> {
    let mut recents = load(paths);
    let before = recents.len();
    recents.retain(|recent| recent.path != gone);
    if recents.len() != before {
        write(paths, &recents)?;
    }
    Ok(recents)
}

/// Empties the list.
pub fn clear(paths: &Paths) -> io::Result<Vec<Recent>> {
    write(paths, &[])?;
    Ok(Vec::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("tendyemu-recents-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn the_cap_is_ten() {
        assert_eq!(MAX_RECENTS, 10);
    }

    #[test]
    fn a_missing_file_is_an_empty_list() {
        let root = scratch("missing");
        assert!(load(&Paths::new(&root)).is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_line_carries_the_system_key_and_mixes_systems() {
        let root = scratch("mix");
        let paths = Paths::new(&root);
        let gb = root.join("a.gb");
        let snes = root.join("b.sfc");
        std::fs::write(&gb, b"x").unwrap();
        std::fs::write(&snes, b"x").unwrap();
        record(&paths, SystemKind::Gb, &gb).unwrap();
        record(&paths, SystemKind::Snes, &snes).unwrap();
        let text = std::fs::read_to_string(paths.recents_file()).unwrap();
        assert!(text.starts_with(&format!("snes\t{}", snes.display())));
        let recents = load(&paths);
        assert_eq!(recents.len(), 2);
        assert_eq!(recents[0].system, SystemKind::Snes);
        assert_eq!(recents[1].system, SystemKind::Gb);
        // A file that has since disappeared is flagged, not dropped.
        std::fs::remove_file(&snes).unwrap();
        assert!(load(&paths)[0].gone);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn reopening_moves_a_rom_to_the_front_without_duplicating_it() {
        let root = scratch("move");
        let paths = Paths::new(&root);
        let first = root.join("a.gb");
        let second = root.join("b.nes");
        record(&paths, SystemKind::Gb, &first).unwrap();
        record(&paths, SystemKind::Nes, &second).unwrap();
        record(&paths, SystemKind::Gb, &first).unwrap();
        let recents = load(&paths);
        assert_eq!(recents.len(), 2);
        assert_eq!(recents[0].path, first);
        assert_eq!(recents[0].system, SystemKind::Gb);
        forget(&paths, &first).unwrap();
        assert_eq!(load(&paths).len(), 1);
        clear(&paths).unwrap();
        assert!(load(&paths).is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_line_with_an_unknown_key_is_skipped() {
        let root = scratch("unknown");
        let paths = Paths::new(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(paths.recents_file(), "gb\tone.gb\nwat\ttwo.gb\n").unwrap();
        let recents = load(&paths);
        assert_eq!(recents.len(), 1);
        assert_eq!(recents[0].path, PathBuf::from("one.gb"));
        let _ = std::fs::remove_dir_all(&root);
    }
}
