//! The command line (issue #84): `--help`, a flag refused by name, and the start options.
//!
//! The parser is the library's, so the start options are applied and checked with no window at
//! all. Help and the refusals run the binary itself: an exit code and the words on stderr are the
//! promise, not a function's return value.

use std::ffi::OsString;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

use tendyemu::app::App;
use tendyemu::display;
use tendyemu::headless;
use tendyemu::paths::Paths;

/// A scratch config folder, removed when the test ends.
struct Scratch {
    root: PathBuf,
}

impl Scratch {
    fn new(name: &str) -> Scratch {
        let root = std::env::temp_dir().join(format!("tendyemu-cli-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("the scratch folder is writable");
        Scratch { root }
    }

    fn paths(&self) -> Paths {
        Paths::new(self.root.clone())
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// A command line, as `main.rs` hands it to the parser.
fn args(words: &[&str]) -> Vec<OsString> {
    words.iter().map(OsString::from).collect()
}

/// The binary this package builds, so a refusal is observed as a user sees it.
fn binary() -> Command {
    Command::new(env!("CARGO_BIN_EXE_tendyemu"))
}

#[test]
fn help_lists_every_flag_including_headless() {
    let out = binary().arg("--help").output().expect("the binary runs");
    assert_eq!(out.status.code(), Some(0), "help is not a failure");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.starts_with("usage: tendyemu"),
        "help starts with usage:\n{text}"
    );
    for flag in [
        "--help",
        "--scale",
        "--fullscreen",
        "--no-audio",
        "--headless",
        "--frames",
        "--blank-frames",
        "--sample",
        "--any-frame",
    ] {
        assert!(text.contains(flag), "--help does not list {flag}:\n{text}");
    }
}

#[test]
fn an_unknown_flag_exits_two_and_names_it() {
    let out = binary().arg("--bogus").output().expect("the binary runs");
    assert_eq!(
        out.status.code(),
        Some(2),
        "a bad line takes the sweep's code"
    );
    let text = String::from_utf8_lossy(&out.stderr);
    assert!(
        text.contains("--bogus"),
        "the refusal does not name the flag:\n{text}"
    );
    assert!(
        text.contains("usage: tendyemu"),
        "the refusal does not say how to find the flags:\n{text}"
    );
}

#[test]
fn a_bad_value_exits_two_and_names_the_flag() {
    for line in [
        &["--scale", "9"][..],
        &["--scale"][..],
        &["--scale", "big"][..],
    ] {
        let out = binary().args(line).output().expect("the binary runs");
        assert_eq!(out.status.code(), Some(2), "{line:?} is a bad line");
        let text = String::from_utf8_lossy(&out.stderr);
        assert!(
            text.contains("--scale"),
            "{line:?} is refused without naming the flag:\n{text}"
        );
    }
}

#[test]
fn the_file_and_the_flags_may_come_in_any_order() {
    let command =
        headless::parse(&args(&["a.gb", "--scale", "2", "--fullscreen"])).expect("a good line");
    assert_eq!(command.path, Some(PathBuf::from("a.gb")));
    assert_eq!(command.scale, Some(2));
    assert!(command.fullscreen);
    assert!(
        !command.no_audio,
        "sound stays on unless --no-audio says otherwise"
    );

    let command = headless::parse(&args(&["--headless", "--frames", "7", "--sample", "roms"]))
        .expect("a good line");
    assert!(command.headless);
    assert_eq!(command.path, Some(PathBuf::from("roms")));
    assert_eq!(command.options.frames, 7);
    assert!(
        !command.options.any_frame,
        "--sample judges the sample frame only"
    );
}

#[test]
fn a_sweep_flag_without_headless_is_refused_by_name() {
    let why = headless::parse(&args(&["--frames", "7", "a.gb"])).expect_err("not a windowed line");
    assert!(why.contains("--headless"), "{why}");
}

#[test]
fn a_second_file_is_refused_by_name() {
    let why = headless::parse(&args(&["a.gb", "b.gb"])).expect_err("one file only");
    assert!(why.contains("b.gb"), "{why}");
}

#[test]
fn scale_starts_the_window_and_is_remembered() {
    let scratch = Scratch::new("scale");
    let mut app = App::new(scratch.paths());
    assert_eq!(
        display::options(&app).scale,
        display::DEFAULT_SCALE,
        "a fresh folder starts at the built-in scale"
    );

    let command = headless::parse(&args(&["--scale", "5"])).expect("a good line");
    command.apply(&mut app).expect("the setting is written");
    assert_eq!(
        display::options(&app).scale,
        5,
        "the window scale is the flag"
    );
    assert!(
        fs::read_to_string(scratch.paths().config_file())
            .expect("config.toml is written")
            .contains("scale = 5"),
        "the key holds the same number as the row"
    );

    // A restart: the same folder, a new app, the same window.
    let restarted = App::new(scratch.paths());
    assert_eq!(
        display::options(&restarted).scale,
        5,
        "the value survives a restart"
    );
}

#[test]
fn fullscreen_starts_fullscreen_and_is_remembered() {
    let scratch = Scratch::new("fullscreen");
    let mut app = App::new(scratch.paths());
    assert!(
        !display::options(&app).fullscreen,
        "a fresh folder is windowed"
    );

    let command = headless::parse(&args(&["--fullscreen"])).expect("a good line");
    command.apply(&mut app).expect("the setting is written");
    assert!(
        display::options(&app).fullscreen,
        "the window fills the screen"
    );

    let restarted = App::new(scratch.paths());
    assert!(
        display::options(&restarted).fullscreen,
        "the value survives a restart"
    );
}

#[test]
fn no_audio_starts_silent_and_writes_no_setting() {
    let scratch = Scratch::new("audio");
    let mut app = App::new(scratch.paths());
    assert!(!app.muted(), "a fresh session has the sound on");

    let command = headless::parse(&args(&["--no-audio"])).expect("a good line");
    assert!(command.no_audio);
    command.apply(&mut app).expect("a good line applies");
    assert!(app.muted(), "the mix starts at zero gain");
    assert!(
        !scratch.paths().config_file().exists(),
        "--no-audio is this run's alone: it writes no setting"
    );
}

#[test]
fn headless_runs_from_the_binary_without_a_window() {
    let missing =
        std::env::temp_dir().join(format!("tendyemu-cli-missing-{}.gb", std::process::id()));
    let out = binary()
        .args(["--headless", "--frames", "1"])
        .arg(&missing)
        .output()
        .expect("the binary runs");
    assert_eq!(
        out.status.code(),
        Some(1),
        "a file that is not there is one error line and exit 1"
    );
    let text = String::from_utf8_lossy(&out.stderr);
    assert!(text.contains("does not exist"), "{text}");
}
