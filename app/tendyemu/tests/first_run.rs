//! The first-run offer (DESIGN §6.5): copy the old apps' files once, and never touch what they left.
//!
//! Every test builds a throwaway config folder with an old app folder beside it, and removes both
//! at the end. The old files are read-only to the code under test; the tests check them byte for
//! byte before and after.

use std::fs;
use std::path::{Path, PathBuf};

use tendy_core::SystemKind;
use tendyemu::app::App;
use tendyemu::config;
use tendyemu::first_run;
use tendyemu::menu::RowAct;
use tendyemu::paths::Paths;

/// A scratch folder holding this app's config folder (`tendyemu`) and old apps beside it.
struct Scratch {
    base: PathBuf,
}

impl Scratch {
    fn new(name: &str) -> Scratch {
        let base =
            std::env::temp_dir().join(format!("tendyemu-first-run-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(base.join("tendyemu")).expect("the scratch folder is writable");
        Scratch { base }
    }

    fn paths(&self) -> Paths {
        Paths::new(self.base.join("tendyemu"))
    }

    /// An old app's folder, created.
    fn old(&self, folder: &str) -> PathBuf {
        let dir = self.base.join(folder);
        fs::create_dir_all(&dir).expect("the old folder is writable");
        dir
    }

    /// Writes `bytes` at `path`, creating its folders.
    fn write(&self, path: &Path, bytes: &[u8]) {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).expect("the parent folder is writable");
        }
        fs::write(path, bytes).expect("the scratch file is writable");
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.base);
    }
}

/// Every file under `dir`, path and bytes, sorted, so a before-and-after comparison sees a change,
/// a move or a new file.
fn snapshot(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut files = Vec::new();
    let mut folders = vec![dir.to_path_buf()];
    while let Some(folder) = folders.pop() {
        for entry in fs::read_dir(&folder).expect("the folder reads").flatten() {
            let path = entry.path();
            if path.is_dir() {
                folders.push(path);
            } else {
                let bytes = fs::read(&path).expect("the file reads");
                files.push((path, bytes));
            }
        }
    }
    files.sort();
    files
}

fn rows_of(app: &App) -> Vec<tendyemu::menu::Row> {
    first_run::rows(app)
}

#[test]
fn copy_never_moves_or_changes_old_files() {
    let scratch = Scratch::new("untouched");
    let gb = scratch.old("gb-rust");
    let rom = scratch.base.join("roms").join("Tetra.gb");
    scratch.write(&rom, b"an image the copy never reads");
    scratch.write(
        &gb.join("config.toml"),
        b"scale = 6\nvolume = 70\nconsole = \"cgb\"\n",
    );
    scratch.write(
        &gb.join("recents.txt"),
        format!("{}\n", rom.display()).as_bytes(),
    );
    scratch.write(&gb.join("saves").join("Tetra [1a2b3c4d].sav"), b"battery");
    scratch.write(&rom.with_extension("state"), b"one state");
    scratch.write(
        &scratch.base.join("roms").join("Tetra-1.state"),
        b"another state",
    );
    let before = snapshot(&gb);

    let mut app = App::new(scratch.paths());
    assert_eq!(rows_of(&app).len(), 3, "the offer has its three rows");
    assert!(first_run::on_row(&mut app, 0, RowAct::Activate));

    assert_eq!(snapshot(&gb), before, "an old file was changed or moved");
    assert!(
        scratch.paths().first_run_done().is_file(),
        "the marker is written"
    );
    let saves = scratch.paths().saves_dir(SystemKind::Gb);
    assert_eq!(fs::read(saves.join("Tetra.sav")).unwrap(), b"battery");
    assert_eq!(
        fs::read(
            scratch
                .paths()
                .states_dir(SystemKind::Gb)
                .join("Tetra.state")
        )
        .unwrap(),
        b"one state"
    );
    assert_eq!(
        fs::read(
            scratch
                .paths()
                .states_dir(SystemKind::Gb)
                .join("Tetra-1.state")
        )
        .unwrap(),
        b"another state"
    );
}

#[test]
fn second_run_does_not_offer() {
    let scratch = Scratch::new("once");
    let gb = scratch.old("gb-rust");
    scratch.write(&gb.join("config.toml"), b"scale = 5\n");

    let mut app = App::new(scratch.paths());
    assert_eq!(rows_of(&app).len(), 3);
    assert!(
        first_run::on_row(&mut app, 1, RowAct::Activate),
        "Skip answers"
    );
    assert!(
        rows_of(&app).is_empty(),
        "the offer is gone after the answer"
    );
    assert!(scratch.paths().first_run_done().is_file());
    // A new session reads the marker, not a memory of the answer.
    let app = App::new(scratch.paths());
    assert!(first_run::rows(&app).is_empty());
}

#[test]
fn old_settings_land_in_system_section() {
    let scratch = Scratch::new("sections");
    let gb = scratch.old("gb-rust");
    scratch.write(
        &gb.join("config.toml"),
        b"scale = 6\nvolume = 70\nstatus_bar = false\nconsole = \"cgb\"\nkeys_file = \"my keys.txt\"\n",
    );
    scratch.write(&gb.join("my keys.txt"), b"up = Up\n");
    let snes = scratch.old("snes-rust");
    scratch.write(&snes.join("config.toml"), b"scale = 4\nmuted = true\n");

    let mut app = App::new(scratch.paths());
    assert!(first_run::on_row(&mut app, 0, RowAct::Activate));

    let paths = scratch.paths();
    let loaded = config::load(&paths);
    // Each app's shared keys are stored under its own section, so neither value wins.
    assert_eq!(loaded.config.get("gb", "scale"), Some("6"));
    assert_eq!(loaded.config.get("snes", "scale"), Some("4"));
    assert_eq!(loaded.config.get("app", "scale"), None);
    assert_eq!(loaded.config.value(SystemKind::Gb, "scale"), Some("6"));
    assert_eq!(loaded.config.value(SystemKind::Snes, "scale"), Some("4"));
    assert_eq!(loaded.config.value(SystemKind::Gb, "volume"), Some("70"));
    assert_eq!(
        loaded.config.value(SystemKind::Snes, "volume"),
        Some("100"),
        "a key the old app did not set keeps its default"
    );
    assert_eq!(loaded.config.get("gb", "console"), Some("cgb"));
    assert_eq!(loaded.config.get("gb", "status_bar"), Some("false"));
    assert_eq!(loaded.config.get("snes", "muted"), Some("true"));

    // The old keys file is copied in, and the setting points at the copy, not the old path.
    let keys = paths.keys_file(SystemKind::Gb);
    assert_eq!(fs::read(&keys).unwrap(), b"up = Up\n");
    assert_eq!(
        loaded.config.get("gb", "keys_file"),
        Some(keys.to_string_lossy().as_ref())
    );

    let text = fs::read_to_string(paths.config_file()).expect("the settings file was written");
    assert!(text.contains("[gb]"), "{text}");
    assert!(text.contains("[snes]"), "{text}");
}

#[test]
fn no_old_apps_no_offer() {
    let scratch = Scratch::new("none");
    let mut app = App::new(scratch.paths());
    assert!(first_run::rows(&app).is_empty(), "nothing to offer");
    app.tick(0.1);
    assert!(app.notice().is_none(), "nothing to announce");
}

#[test]
fn nes_folder_asked_when_not_found() {
    let scratch = Scratch::new("nes");
    let gb = scratch.old("gb-rust");
    scratch.write(&gb.join("config.toml"), b"scale = 5\n");
    let mut app = App::new(scratch.paths());

    let rows = rows_of(&app);
    assert_eq!(rows.len(), 3);
    assert!(rows[2].label.contains("NES"), "{}", rows[2].label);
    assert_eq!(rows[2].value, "not set", "the folder is not known yet");
    // Without a window there is no dialog to show, so the row does nothing.
    assert!(!first_run::on_row(&mut app, 2, RowAct::Activate));
    assert_eq!(first_run::nes_folder(app.paths()), None);

    // Pointing the offer at the folder is the step the dialog makes.
    let nes = scratch.old("nes-app");
    scratch.write(
        &nes.join("config.toml"),
        b"scale = 2\npalette = \"composite\"\n",
    );
    first_run::remember_nes(app.paths(), &nes);
    assert_eq!(rows_of(&app)[2].value, "nes-app");
    assert!(first_run::on_row(&mut app, 0, RowAct::Activate));
    assert_eq!(
        config::load(app.paths()).config.get("nes", "scale"),
        Some("2")
    );
}

#[test]
fn battery_file_copied_into_system_folder() {
    let scratch = Scratch::new("battery");

    // The Game Boy app kept the battery in its own folder, under the stem and CRC-32.
    let gb = scratch.old("gb-rust");
    let gb_rom = scratch.base.join("roms").join("Tetra.gb");
    scratch.write(&gb_rom, b"gb image");
    scratch.write(
        &gb.join("saves").join("Tetra [deadbeef].sav"),
        b"gb battery",
    );
    scratch.write(
        &gb.join("recents.txt"),
        format!("{}\n", gb_rom.display()).as_bytes(),
    );

    // The Super NES app wrote the battery beside the cartridge.
    let snes = scratch.old("snes-rust");
    let snes_rom = scratch.base.join("roms").join("Island.sfc");
    scratch.write(&snes_rom, b"snes image");
    scratch.write(&snes_rom.with_extension("srm"), b"snes battery");
    scratch.write(
        &snes.join("recents.txt"),
        format!("{}\n", snes_rom.display()).as_bytes(),
    );

    // The NES app does the same, and its folder is the one the offer has to find.
    let nes = scratch.old("nes-app");
    let nes_rom = scratch.base.join("roms").join("Kart.nes");
    scratch.write(&nes_rom, b"nes image");
    scratch.write(&nes_rom.with_extension("sav"), b"nes battery");
    scratch.write(
        &nes.join("recents.txt"),
        format!("{}\n", nes_rom.display()).as_bytes(),
    );

    let paths = scratch.paths();
    first_run::remember_nes(&paths, &nes);
    let mut app = App::new(paths.clone());
    assert!(first_run::on_row(&mut app, 0, RowAct::Activate));

    assert_eq!(
        fs::read(paths.saves_dir(SystemKind::Gb).join("Tetra.sav")).unwrap(),
        b"gb battery"
    );
    assert_eq!(
        fs::read(paths.saves_dir(SystemKind::Snes).join("Island.srm")).unwrap(),
        b"snes battery"
    );
    assert_eq!(
        fs::read(paths.saves_dir(SystemKind::Nes).join("Kart.sav")).unwrap(),
        b"nes battery"
    );
    // The old batteries are still where the old apps left them.
    assert_eq!(
        fs::read(gb.join("saves").join("Tetra [deadbeef].sav")).unwrap(),
        b"gb battery"
    );
    assert_eq!(
        fs::read(snes_rom.with_extension("srm")).unwrap(),
        b"snes battery"
    );
    assert_eq!(
        fs::read(nes_rom.with_extension("sav")).unwrap(),
        b"nes battery"
    );
}
