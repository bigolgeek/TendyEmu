//! The Super NES front end's own behaviours (DESIGN §5.5, §8 #27).
//!
//! These four tests are the ported half of the old app's front-end suite: the battery body's
//! rules (the existing tests cover where the file goes, not what it holds) and the config
//! scale clamp. Everything else the old suite pinned is carried by a shared test, and
//! `baselines/dropped-tests-snes.txt` names the replacement for each one.

use std::fs;
use std::path::PathBuf;

use tendyemu::app::App;
use tendyemu::config;
use tendyemu::display;
use tendyemu::paths::Paths;

/// A scratch config folder, removed when the test ends.
struct Scratch {
    root: PathBuf,
}

impl Scratch {
    fn new(name: &str) -> Scratch {
        let root =
            std::env::temp_dir().join(format!("tendyemu-snes-front-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("the scratch folder is writable");
        Scratch { root }
    }

    fn paths(&self) -> Paths {
        Paths::new(self.root.clone())
    }

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

/// A 32 KiB LoROM cartridge. With `battery`, cart type 2 and an 8 KiB SRAM chip, as the old
/// front-end fixture had.
fn snes_rom(battery: bool) -> Vec<u8> {
    let mut rom = vec![0u8; 0x8000];
    let at = 0x7FC0;
    rom[at..at + 21].copy_from_slice(b"PARITY SNES TEST ROM ");
    rom[at + 0x15] = 0x20;
    rom[at + 0x16] = if battery { 0x02 } else { 0x00 };
    rom[at + 0x17] = 0x05;
    rom[at + 0x18] = if battery { 0x03 } else { 0x00 };
    rom
}

/// Opens a battery cartridge and hands back its machine, ready to load a body into.
fn battery_app(scratch: &Scratch) -> App {
    let mut app = App::new(scratch.paths());
    let rom = scratch.rom("battery.sfc", &snes_rom(true));
    app.open(&rom).expect("a battery cartridge opens");
    app
}

#[test]
fn ram_round_trip() {
    let scratch = Scratch::new("ram-round-trip");
    let mut app = battery_app(&scratch);
    let body = app
        .machine()
        .and_then(|machine| machine.battery_body())
        .expect("the cartridge carries an 8 KiB SRAM chip");
    assert_eq!(body.len(), 8 * 1024, "SRAM size code 3 is 8 KiB");

    // Contents the game wrote, loaded back as the app loads a save file.
    let mut used = body.clone();
    used[0] = 0x5A;
    used[8191] = 0xA5;
    app.machine_mut()
        .expect("the game is open")
        .load_battery_body(&used, 0)
        .expect("a body of the chip's size loads");
    assert_eq!(
        app.machine()
            .and_then(|machine| machine.battery_body())
            .expect("the body is still there"),
        used,
        "every byte the game wrote survives the round trip"
    );
}

#[test]
fn short_file_loads_into_the_front() {
    let scratch = Scratch::new("short");
    let mut app = battery_app(&scratch);
    let short = vec![0x11u8; 1024];
    app.machine_mut()
        .expect("the game is open")
        .load_battery_body(&short, 0)
        .expect("a short body loads");
    let body = app
        .machine()
        .and_then(|machine| machine.battery_body())
        .expect("the cartridge still has its body");
    assert_eq!(&body[..1024], &short[..], "the file fills the front");
    assert!(
        body[1024..].iter().all(|byte| *byte == 0),
        "the rest of the chip a short file does not name stays zero"
    );
}

#[test]
fn long_file_is_truncated() {
    let scratch = Scratch::new("long");
    let mut app = battery_app(&scratch);
    let long = vec![0x22u8; 16 * 1024];
    app.machine_mut()
        .expect("the game is open")
        .load_battery_body(&long, 0)
        .expect("a long body loads");
    let body = app
        .machine()
        .and_then(|machine| machine.battery_body())
        .expect("the cartridge still has its body");
    assert_eq!(body.len(), 8 * 1024, "the chip's size wins");
    assert!(
        body.iter().all(|byte| *byte == 0x22),
        "the front of a long file is what the chip keeps"
    );
}

#[test]
fn scale_is_clamped() {
    let scratch = Scratch::new("scale");
    let paths = scratch.paths();
    config::write_value(&paths, "snes", "scale", "99").expect("the config is writable");
    let mut app = App::new(paths.clone());
    let rom = scratch.rom("scale.sfc", &snes_rom(false));
    app.open(&rom).expect("the cartridge opens");
    assert_eq!(
        display::options(&app).scale,
        display::MAX_SCALE,
        "a file asking for more than the rows offer runs at the top step"
    );

    config::write_value(&paths, "snes", "scale", "0").expect("the config is writable");
    assert_eq!(
        display::options(&app).scale,
        display::MIN_SCALE,
        "and a zero runs at the bottom step, not at a zero-sized window"
    );
}
