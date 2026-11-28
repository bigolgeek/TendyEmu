//! A battery file another program holds open (DESIGN §6.3; issue #89).
//!
//! Windows refuses the rename over a save that a virus scanner, an editor or a thumbnailer has
//! open, and refuses the temporary when it holds that instead. Both clear in milliseconds, so the
//! writer retries for about a second before it reports, and it never truncates the old save first.

use std::fs;
use std::path::{Path, PathBuf};

use tendy_core::SystemKind;
use tendyemu::battery;
use tendyemu::paths::Paths;

/// A scratch folder, removed when the test ends.
struct Scratch {
    root: PathBuf,
}

impl Scratch {
    fn new(name: &str) -> Scratch {
        let root =
            std::env::temp_dir().join(format!("tendyemu-battery-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("the scratch folder is writable");
        Scratch { root }
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// Holds `path` the way another program can: open for reading and sharing nothing, so every other
/// open, delete or rename of it is refused until the handle drops.
#[cfg(windows)]
fn hold(path: &Path) -> fs::File {
    use std::os::windows::fs::OpenOptionsExt;
    fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(path)
        .expect("the save can be held open")
}

#[test]
fn an_unlocked_battery_file_writes() {
    let scratch = Scratch::new("unlocked");
    let paths = Paths::new(&scratch.root);
    let rom = Path::new("Zelda.sfc");
    let file = battery::write(&paths, SystemKind::Snes, rom, &[7, 7, 7])
        .expect("an unlocked save writes at once");
    assert_eq!(fs::read(&file).unwrap(), vec![7, 7, 7]);
    assert!(!file.with_extension("tmp").exists());
}

#[cfg(windows)]
#[test]
fn a_locked_battery_file_is_retried_then_reported() {
    use std::time::{Duration, Instant};

    let scratch = Scratch::new("locked");
    let paths = Paths::new(&scratch.root);
    let rom = scratch.root.join("Zelda.sfc");
    let file = battery::write(&paths, SystemKind::Snes, &rom, &[1, 2, 3, 4])
        .expect("the first save writes");

    let held = hold(&file);
    let started = Instant::now();
    let result = battery::write(&paths, SystemKind::Snes, &rom, &[9, 9, 9, 9]);
    let waited = started.elapsed();
    drop(held);

    assert!(result.is_err(), "a save that stays held is reported");
    assert!(
        waited >= Duration::from_millis(500),
        "the write was retried for about a second, not given up at once: {waited:?}"
    );
    assert_eq!(
        fs::read(&file).unwrap(),
        vec![1, 2, 3, 4],
        "the save that was already there is never truncated"
    );
    assert!(
        !file.with_extension("tmp").exists(),
        "a failed write leaves no temporary behind"
    );
}

#[cfg(windows)]
#[test]
fn a_lock_that_clears_is_waited_out() {
    use std::sync::mpsc;
    use std::time::Duration;

    let scratch = Scratch::new("clears");
    let paths = Paths::new(&scratch.root);
    let rom = scratch.root.join("Zelda.sfc");
    let file =
        battery::write(&paths, SystemKind::Snes, &rom, &[1, 2, 3]).expect("the first save writes");

    // A second thread holds the file for a moment, then lets go, like a scanner finishing a read.
    let (ready, opened) = mpsc::channel();
    let holder = {
        let file = file.clone();
        std::thread::spawn(move || {
            let held = hold(&file);
            ready.send(()).expect("the test is still waiting");
            std::thread::sleep(Duration::from_millis(150));
            drop(held);
        })
    };
    opened.recv().expect("the holder took the file");

    battery::write(&paths, SystemKind::Snes, &rom, &[4, 5, 6])
        .expect("the retry waits until the holder lets go");
    holder.join().expect("the holder ends");

    assert_eq!(fs::read(&file).unwrap(), vec![4, 5, 6]);
    assert!(!file.with_extension("tmp").exists());
}
