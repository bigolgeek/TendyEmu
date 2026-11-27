//! The battery save: one writer for every core (DESIGN §6.1, §6.3).
//!
//! The body comes from `Machine::battery_body`, the file name from the ROM's stem and the
//! system's extension as each app writes it today (`.sav` for GB/GBA/NES, `.srm` for SNES), the
//! folder from [`crate::paths`]. A write goes to `<file>.tmp` and is then renamed over the real
//! file, so a crash mid-write leaves the previous save intact.

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use tendy_core::SystemKind;

use crate::paths::Paths;

/// The extension each app writes today.
pub fn extension(system: SystemKind) -> &'static str {
    match system {
        SystemKind::Gb | SystemKind::Gba | SystemKind::Nes => "sav",
        SystemKind::Snes => "srm",
    }
}

/// `<root>/<system>/saves/<stem>.<ext>` (§6.3).
pub fn save_path(paths: &Paths, system: SystemKind, rom: &Path) -> PathBuf {
    let stem = rom.file_stem().unwrap_or_default().to_string_lossy();
    paths
        .saves_dir(system)
        .join(format!("{stem}.{}", extension(system)))
}

/// The save body to load for `rom`: the per-system file, else one of the same stem and extension
/// beside the ROM (what the old apps wrote, §6.3).
pub fn read(paths: &Paths, system: SystemKind, rom: &Path) -> Option<Vec<u8>> {
    let ext = extension(system);
    let candidates = [save_path(paths, system, rom), rom.with_extension(ext)];
    candidates.iter().find_map(|path| std::fs::read(path).ok())
}

/// Writes `body` to the per-system folder and returns the file's path.
pub fn write(paths: &Paths, system: SystemKind, rom: &Path, body: &[u8]) -> io::Result<PathBuf> {
    let file = save_path(paths, system, rom);
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    write_atomic(&file, body)?;
    Ok(file)
}

/// Writes through `<path>.tmp`, then renames it over `path`.
pub fn write_atomic(path: &Path, data: &[u8]) -> io::Result<()> {
    let tmp = path.with_extension("tmp");
    {
        let mut file = std::fs::File::create(&tmp)?;
        file.write_all(data)?;
        file.sync_all()?;
    }
    std::fs::rename(&tmp, path)
}

/// Wall-clock milliseconds since the UNIX epoch, for the cores' real-time clocks.
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("tendyemu-battery-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn the_extension_is_each_app_s_own() {
        assert_eq!(extension(SystemKind::Gb), "sav");
        assert_eq!(extension(SystemKind::Gba), "sav");
        assert_eq!(extension(SystemKind::Nes), "sav");
        assert_eq!(extension(SystemKind::Snes), "srm");
    }

    #[test]
    fn a_save_lands_in_the_system_folder_under_the_rom_s_stem() {
        let root = scratch("path");
        let paths = Paths::new(&root);
        let file = save_path(&paths, SystemKind::Snes, Path::new("games/Zelda.sfc"));
        assert_eq!(file, root.join("snes").join("saves").join("Zelda.srm"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_write_leaves_no_temporary_file_behind() {
        let root = scratch("write");
        let paths = Paths::new(&root);
        let rom = Path::new("Zelda.gb");
        let file = write(&paths, SystemKind::Gb, rom, &[1, 2, 3]).expect("the write must succeed");
        assert_eq!(std::fs::read(&file).unwrap(), vec![1, 2, 3]);
        assert!(!file.with_extension("tmp").exists());
        // The per-system file wins on read, and a body beside the ROM is still found.
        assert_eq!(read(&paths, SystemKind::Gb, rom).unwrap(), vec![1, 2, 3]);
        assert_eq!(read(&paths, SystemKind::Gb, Path::new("missing.gb")), None);
        let _ = std::fs::remove_dir_all(&root);
    }
}
