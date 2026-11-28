//! Runs a file, or a folder of files, through detection and its core without a window (DESIGN
//! §6.1, §8; issue #28).
//!
//! The window and the device calls are the binary's job. This module is the same detection, the
//! same `loader::build` and the same `Machine::run_frame` as the window loop, with the picture
//! counted instead of drawn. A folder run is the library sweep: one verdict per file, and a tally
//! per system — draws, blank, refused — so the maintainers' library can be compared with the
//! counts each source project last published.
//!
//! The verdict is the frame at N, the way the source sweeps judged it: more than one colour is a
//! picture, one colour is a blank. A row that is one colour at N is given the longer horizon
//! (`Options::long`) before the word `blank` is written, because a slow engine is not a dead
//! machine. `Options::any_frame` is the Super NES source sweep's rule, which counted a cartridge
//! that drew at frame 113 as drawing; the Game Boy and Advance sweeps sampled exactly at N, so
//! they set it false.
//!
//! Nothing here writes: no settings, no recents, no battery, no screenshots. A sweep leaves the
//! tree exactly as it found it, which is why results with file paths are only ever printed.

use std::collections::HashSet;
use std::ffi::OsString;
use std::fmt;
use std::path::{Path, PathBuf};

use tendy_core::{Frame, Machine, SystemKind};
use tendy_detect::ArchiveKind;

/// How far a run goes, and which frames count as the sample.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Options {
    /// The frame every row is judged on.
    pub frames: u32,
    /// The longer horizon a one-colour row is given before it is called blank.
    pub long: u32,
    /// Judge on any frame up to `long`, not only on the samples (the Super NES sweep's rule).
    pub any_frame: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            frames: 600,
            long: 1800,
            any_frame: true,
        }
    }
}

/// What a run saw.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// More than one colour on `at`. `colours` is the exact count when `at` is the sample frame;
    /// it is 0 when `at` names an earlier frame (the picture was found, not counted).
    Draws { at: u32, colours: usize },
    /// One colour on every frame of the run.
    Blank,
    /// Detection or the core said no; the text starts `refused: `.
    Refused(String),
    /// The core panicked; the text is the panic's message.
    Crashed(String),
}

impl Verdict {
    /// The one-line form the sweep prints.
    pub fn detail(&self) -> String {
        match self {
            Verdict::Draws { at, colours } if *colours > 1 => {
                format!("draws ({colours} colours at {at})")
            }
            Verdict::Draws { at, .. } => format!("draws (first picture at {at})"),
            Verdict::Blank => "blank".to_string(),
            Verdict::Refused(why) => why.clone(),
            Verdict::Crashed(why) => format!("crash: {why}"),
        }
    }
}

/// One file's run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Outcome {
    /// The file relative to the swept folder, with `/` separators (never an absolute path).
    pub file: String,
    /// The system detection named; `None` when detection itself refused before naming one.
    pub system: Option<SystemKind>,
    /// The cartridge title the core reports; empty when no machine was built.
    pub title: String,
    /// Frames actually run.
    pub frames: u32,
    /// The core's frame fold after the last frame, in hex in the report.
    pub checksum: u64,
    /// The last picture's size, in pixels.
    pub width: u32,
    pub height: u32,
    /// What the run is.
    pub verdict: Verdict,
}

/// Counts for one system, or for files detection could not name.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Tally {
    /// More than one colour on the frame the verdict was judged on.
    pub draws: usize,
    /// One colour on every sampled frame.
    pub blank: usize,
    /// Never a machine: detection or the core refused the file.
    pub refused: usize,
    /// The core panicked.
    pub crashed: usize,
}

impl Tally {
    /// Files this tally covers.
    pub fn total(&self) -> usize {
        self.draws + self.blank + self.refused + self.crashed
    }

    fn add(&mut self, verdict: &Verdict) {
        match verdict {
            Verdict::Draws { .. } => self.draws += 1,
            Verdict::Blank => self.blank += 1,
            Verdict::Refused(_) => self.refused += 1,
            Verdict::Crashed(_) => self.crashed += 1,
        }
    }
}

/// Every file one sweep ran, in path order, and the tallies that follow from them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Report {
    /// The options every row ran under.
    pub options: Options,
    /// One row per file, sorted by `file`.
    pub outcomes: Vec<Outcome>,
}

impl Report {
    /// Builds a report from finished rows, re-sorted by name so two runs compare line for line.
    pub fn new(options: Options, mut outcomes: Vec<Outcome>) -> Report {
        outcomes.sort_by(|a, b| a.file.cmp(&b.file));
        Report { options, outcomes }
    }

    /// The tally for one system, or for the files detection could not name.
    pub fn tally(&self, system: Option<SystemKind>) -> Tally {
        let mut tally = Tally::default();
        for outcome in &self.outcomes {
            if outcome.system == system {
                tally.add(&outcome.verdict);
            }
        }
        tally
    }

    /// The systems present, in menu order, with `None` (unnamed) last.
    pub fn systems(&self) -> Vec<Option<SystemKind>> {
        let mut systems: Vec<Option<SystemKind>> = SystemKind::ALL
            .into_iter()
            .map(Some)
            .filter(|system| self.outcomes.iter().any(|row| row.system == *system))
            .collect();
        if self.outcomes.iter().any(|row| row.system.is_none()) {
            systems.push(None);
        }
        systems
    }

    /// The rows in one tally cell, for a mismatch report.
    pub fn files(&self, system: Option<SystemKind>, verdict: VerdictKind) -> Vec<&Outcome> {
        self.outcomes
            .iter()
            .filter(|row| row.system == system && VerdictKind::of(&row.verdict) == verdict)
            .collect()
    }
}

/// The five-way bucket a verdict falls in, so a report can name the files in one cell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VerdictKind {
    Draws,
    Blank,
    Refused,
    Crashed,
}

impl VerdictKind {
    fn of(verdict: &Verdict) -> VerdictKind {
        match verdict {
            Verdict::Draws { .. } => VerdictKind::Draws,
            Verdict::Blank => VerdictKind::Blank,
            Verdict::Refused(_) => VerdictKind::Refused,
            Verdict::Crashed(_) => VerdictKind::Crashed,
        }
    }
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for row in &self.outcomes {
            writeln!(
                f,
                "{} | {} | {} | {} | checksum {:016x}",
                row.file,
                system_key(row.system),
                row.title.replace('|', "/"),
                row.verdict.detail(),
                row.checksum
            )?;
        }
        writeln!(f)?;
        let mut total = Tally::default();
        for system in self.systems() {
            let tally = self.tally(system);
            total.draws += tally.draws;
            total.blank += tally.blank;
            total.refused += tally.refused;
            total.crashed += tally.crashed;
            writeln!(f, "{}", tally_line(system_key(system), tally))?;
        }
        write!(f, "{}", tally_line("total", total))
    }
}

/// `gb    draws 1589  blank 12  refused 8  crash 0  total 1609`.
fn tally_line(name: &str, tally: Tally) -> String {
    format!(
        "{name:<5} draws {}  blank {}  refused {}  crash {}  total {}",
        tally.draws,
        tally.blank,
        tally.refused,
        tally.crashed,
        tally.total()
    )
}

/// `gb`, `gba`, `nes`, `snes`, or `?` for files detection could not name.
fn system_key(system: Option<SystemKind>) -> &'static str {
    system.map_or("?", SystemKind::key)
}

/// Whether a file name is one the sweep takes: a ROM extension detection knows, or an archive.
///
/// Save files (`.srm`, `.sav`), notes and pictures sit beside ROMs in a library and are not the
/// sweep's subject; the source sweeps listed only cartridge extensions for the same reason.
pub fn is_rom_name(name: &str) -> bool {
    if ArchiveKind::from_name(name).is_some() {
        return true;
    }
    name.rsplit_once('.')
        .and_then(|(_, extension)| tendy_detect::extension::system_of(extension))
        .is_some()
}

/// Reads `path`, detects what it is, builds the core and runs it for `options.frames` (and to
/// `options.long` when the sample is one colour), all inside `catch_unwind` so one bad cartridge
/// is one `crash` row and not the end of the sweep.
pub fn run_file(path: &Path, options: &Options) -> Outcome {
    let file = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned());
    let mut outcome = Outcome {
        file: file.clone(),
        system: None,
        title: String::new(),
        frames: 0,
        checksum: 0,
        width: 0,
        height: 0,
        verdict: Verdict::Blank,
    };

    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(why) => {
            outcome.verdict = Verdict::Refused(format!("refused: cannot read {file}: {why}"));
            return outcome;
        }
    };
    let (entry, detected) = match tendy_detect::open(&file, bytes) {
        Ok(opened) => opened,
        Err(refusal) => {
            outcome.verdict = Verdict::Refused(refusal.to_string());
            return outcome;
        }
    };
    outcome.system = Some(detected.system);
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut machine = crate::loader::build(&entry, &detected)?;
        Ok::<RunData, String>(run(&mut *machine, options))
    })) {
        Ok(Ok(data)) => {
            outcome.title = data.title;
            outcome.frames = data.frames;
            outcome.checksum = data.checksum;
            outcome.width = data.width;
            outcome.height = data.height;
            outcome.verdict = data.verdict;
        }
        Ok(Err(why)) => outcome.verdict = Verdict::Refused(why),
        Err(payload) => outcome.verdict = Verdict::Crashed(panic_text(&*payload)),
    }
    outcome
}

/// What one run produced before it became an `Outcome`.
struct RunData {
    title: String,
    frames: u32,
    checksum: u64,
    width: u32,
    height: u32,
    verdict: Verdict,
}

fn run(machine: &mut dyn Machine, options: &Options) -> RunData {
    let sample = options.frames.max(1);
    let horizon = options.long.max(sample);
    let mut frames = 0u32;
    let mut first_picture: Option<u32> = None;

    // The sample frame: every row runs at least this far, whatever the horizon.
    while frames < sample {
        machine.run_frame();
        frames += 1;
        if options.any_frame && first_picture.is_none() && has_picture(machine.frame()) {
            first_picture = Some(frames);
        }
    }
    let colours_at_sample = colours(machine.frame());

    // A one-colour sample is given the longer horizon, unless `long` is the sample itself. The
    // any-frame rule always finishes the horizon, so two runs of one file are comparable.
    if horizon > frames && (options.any_frame || colours_at_sample <= 1) {
        while frames < horizon {
            machine.run_frame();
            frames += 1;
            if options.any_frame && first_picture.is_none() && has_picture(machine.frame()) {
                first_picture = Some(frames);
            }
        }
    }

    let verdict = if colours_at_sample > 1 {
        Verdict::Draws {
            at: sample,
            colours: colours_at_sample,
        }
    } else if let Some(at) = first_picture {
        Verdict::Draws { at, colours: 0 }
    } else if frames > sample && colours(machine.frame()) > 1 {
        // The sample was one colour and the longer horizon drew: the source sweeps counted that
        // as drawing, not as blank.
        Verdict::Draws {
            at: frames,
            colours: colours(machine.frame()),
        }
    } else {
        Verdict::Blank
    };

    let (width, height) = {
        let frame = machine.frame();
        (frame.width, frame.height)
    };
    RunData {
        title: machine.cart_identity().title,
        frames,
        checksum: machine.frame_checksum(),
        width,
        height,
        verdict,
    }
}

/// Distinct pixel values on a finished frame, exactly. Called on sample frames only.
fn colours(frame: Frame<'_>) -> usize {
    frame
        .rgba
        .as_chunks::<{ Frame::BYTES_PER_PIXEL }>()
        .0
        .iter()
        .map(|pixel| u32::from_le_bytes(*pixel))
        .collect::<HashSet<u32>>()
        .len()
}

/// Whether the frame holds more than one colour. Stops at the second one: a sweep asks the
/// question thousands of times, and a picture is almost always two pixels away.
fn has_picture(frame: Frame<'_>) -> bool {
    let pixels = frame.rgba.as_chunks::<{ Frame::BYTES_PER_PIXEL }>().0;
    let Some((first, rest)) = pixels.split_first() else {
        return false;
    };
    rest.iter().any(|pixel| pixel != first)
}

/// A panic's message, whatever type it was raised with.
fn panic_text(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(text) = payload.downcast_ref::<&str>() {
        (*text).to_string()
    } else if let Some(text) = payload.downcast_ref::<String>() {
        text.clone()
    } else {
        "panic".to_string()
    }
}

/// Sweeps every ROM-looking file under `dir`, in path order.
pub fn sweep(dir: &Path, options: &Options) -> std::io::Result<Report> {
    let mut files = Vec::new();
    collect(dir, dir, &mut files)?;
    files.sort();
    let outcomes = files
        .iter()
        .map(|(path, relative)| {
            let mut outcome = run_file(path, options);
            outcome.file = relative.clone();
            outcome
        })
        .collect();
    Ok(Report::new(*options, outcomes))
}

/// One folder of files, recursively, as `(path, path relative to base with / separators)`.
fn collect(dir: &Path, base: &Path, out: &mut Vec<(PathBuf, String)>) -> std::io::Result<()> {
    let mut entries: Vec<std::fs::DirEntry> =
        std::fs::read_dir(dir)?.filter_map(Result::ok).collect();
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let path = entry.path();
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() {
            // An unreadable subfolder is not the sweep's subject; the files it can read still run.
            // `file_type` does not follow links, so a symlinked folder (a Windows junction, say) is
            // skipped: `find` does not follow either, and one pointing at its own parent would not end.
            let _ = collect(&path, base, out);
        } else if entry.file_name().to_str().is_some_and(is_rom_name) {
            let relative = path
                .strip_prefix(base)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            out.push((path, relative));
        }
    }
    Ok(())
}

/// The words `tendyemu` prints when it is asked for help.
pub const USAGE: &str = "usage: tendyemu --headless --frames N [--blank-frames N] [--sample|--any-frame] <file-or-folder>";

/// The headless command: `tendyemu --headless --frames N <file-or-folder>` (DESIGN §6.1).
///
/// `main.rs` hands its arguments here when `--headless` is the first one; the exit code is 0 when
/// the report was printed (refusals are rows, not errors), 2 for a bad command line and 1 when the
/// folder cannot be read.
pub fn cli(args: &[OsString]) -> i32 {
    let mut options = Options::default();
    let mut path: Option<PathBuf> = None;
    let mut words = args.iter().map(|arg| arg.to_string_lossy().into_owned());
    while let Some(word) = words.next() {
        match word.as_str() {
            "--headless" => {}
            "--sample" => options.any_frame = false,
            "--any-frame" => options.any_frame = true,
            "--frames" | "--blank-frames" => {
                let Some(value) = words.next().and_then(|value| value.parse::<u32>().ok()) else {
                    eprintln!("{USAGE}");
                    return 2;
                };
                if word == "--frames" {
                    options.frames = value;
                } else {
                    options.long = value;
                }
            }
            other if other.starts_with('-') => {
                eprintln!("{USAGE}");
                return 2;
            }
            other if path.is_none() => path = Some(PathBuf::from(other)),
            _ => {
                eprintln!("{USAGE}");
                return 2;
            }
        }
    }
    let Some(path) = path else {
        eprintln!("{USAGE}");
        return 2;
    };
    if !path.exists() {
        eprintln!("sweep: {} does not exist", path.display());
        return 1;
    }
    if path.is_dir() {
        match sweep(&path, &options) {
            Ok(report) => {
                println!("{report}");
                0
            }
            Err(why) => {
                eprintln!("sweep: cannot read {}: {why}", path.display());
                1
            }
        }
    } else {
        let report = Report::new(options, vec![run_file(&path, &options)]);
        println!("{report}");
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(pixels: &[[u8; 4]]) -> Vec<u8> {
        pixels.iter().flatten().copied().collect()
    }

    #[test]
    fn a_blank_frame_has_one_colour_and_a_picture_two() {
        let blank = frame(&[[1, 2, 3, 0xFF]; 9]);
        assert_eq!(colours(Frame::new(3, 3, &blank)), 1);
        assert!(!has_picture(Frame::new(3, 3, &blank)));
        let mut picture = blank.clone();
        picture[8] = 9;
        assert_eq!(colours(Frame::new(3, 3, &picture)), 2);
        assert!(has_picture(Frame::new(3, 3, &picture)));
    }

    #[test]
    fn the_sweep_takes_roms_and_archives_not_saves_or_notes() {
        for name in ["a.gb", "a.GBC", "a.nes", "a.sfc", "a.zip", "a.7z", "a.fds"] {
            assert!(is_rom_name(name), "{name}");
        }
        for name in ["a.srm", "a.sav", "notes.txt", "a.png", "no-extension"] {
            assert!(!is_rom_name(name), "{name}");
        }
    }

    #[test]
    fn a_bad_command_line_is_two_and_says_how_to_use_it() {
        assert_eq!(
            cli(&["--headless".into(), "--frames".into(), "7".into()]),
            2
        );
        assert_eq!(
            cli(&[
                "--headless".into(),
                "--frames".into(),
                "nope".into(),
                "x.gb".into()
            ]),
            2
        );
        assert_eq!(cli(&["--headless".into(), "--bogus".into()]), 2);
        // A path that is not there is one error line and exit 1, not a report of one refusal.
        assert_eq!(
            cli(&[
                "--headless".into(),
                "--frames".into(),
                "1".into(),
                "no-such-file.gb".into()
            ]),
            1
        );
    }

    #[test]
    fn the_tally_counts_each_verdict_once() {
        let mut tally = Tally::default();
        tally.add(&Verdict::Draws { at: 1, colours: 2 });
        tally.add(&Verdict::Blank);
        tally.add(&Verdict::Refused("refused: x".into()));
        tally.add(&Verdict::Crashed("boom".into()));
        assert_eq!(
            tally,
            Tally {
                draws: 1,
                blank: 1,
                refused: 1,
                crashed: 1
            }
        );
        assert_eq!(tally.total(), 4);
    }
}
