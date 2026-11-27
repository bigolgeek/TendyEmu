//! FM2 movie format: header lines ("key value") followed by one line per frame,
//! `|c|RLDUTSBA|RLDUTSBA||`, where `c` is the frame's command bits (bit 0 = soft
//! reset, bit 1 = power cycle) and each pad string is one button per position,
//! in FM2 order R L D U T S B A. A position that is not `.` or a space is a
//! pressed button; position `i` maps to bit `7 - i`, which matches the standard
//! joypad bits (`BUTTON_RIGHT = 0x80 .. BUTTON_A = 0x01`, the FM2 order is the
//! joypad shift order reversed).
//!
//! BizHawk's `.bk2` is a zip holding the same per-frame input, in `Input Log.txt`: a
//! `LogKey:` line names the button columns (its `#` marks where one controller's group starts)
//! and every line after it that starts with `|` is one frame. A column is one character per
//! button — `.` when it is not pressed, any other character when it is — except an analog one,
//! which is a 5-character number in front of a comma. [`parse_bk2`] reads that into the same
//! [`Movie`] an FM2 parses into, so `--movie game.bk2` plays through the existing [`Player`]
//! instead of a second playback path. Only import is implemented: writing `.bk2`, loading the
//! `Core.bin` savestate a savestate-anchored movie needs, and BizHawk's lag/hash checking are
//! not.

use std::io::Read;
use std::path::Path;

use crate::cart::{Region, RomData};
use crate::checksum;
use crate::console::ConsoleConfig;
use crate::controller::{
    BUTTON_A, BUTTON_B, BUTTON_DOWN, BUTTON_LEFT, BUTTON_RIGHT, BUTTON_SELECT, BUTTON_START,
    BUTTON_UP,
};
use crate::nes::{Nes, PowerOn};

/// One recorded frame: the command bits for `|c|` and both pad values.
///
/// `pads[0]` is the first pad, `pads[1]` the second. Each is the standard
/// NES button mask; FM2 records every bit, including the ones the standard
/// controller never sees.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MovieFrame {
    /// Command bits: bit 0 = soft reset, bit 1 = power cycle.
    pub commands: u8,
    /// Both pad masks, joypad bit order (`BUTTON_A` = bit 0 .. `BUTTON_RIGHT` = bit 7).
    pub pads: [u8; 2],
}

/// A parsed FM2 movie: ordered header key/value pairs and the frame list.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Movie {
    /// Header lines in file order, split at the first space.
    pub header: Vec<(String, String)>,
    /// One entry per frame line, in file order.
    pub frames: Vec<MovieFrame>,
}

/// What can go wrong while parsing a movie.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MovieError {
    /// A malformed line (1-based line number in the file it came from: an FM2 counts header
    /// lines, a `.bk2` counts the lines of its `Input Log.txt`).
    BadLine(usize),
    /// The movie contains a power-cycle command, which cannot be replayed.
    PowerCycleUnsupported,
    /// A `.bk2` archive could not be read: not a zip, an unreadable entry, or an entry that is
    /// not UTF-8. The message is the underlying error, which names the entry where there is one.
    Bk2Archive(String),
    /// A `.bk2` archive has no `Input Log.txt`, so it holds no frames to play.
    Bk2NoInputLog,
    /// A `.bk2` archive holds the savestate its first frame continues from, so that frame is
    /// not frame 0 of the game and playing the archive from power-on would desync.
    Bk2Savestate,
    /// A `.bk2` input log has no `LogKey:` line, so its columns have no button names and
    /// cannot be decoded by position: the order changed between BizHawk cores.
    Bk2NoLogKey,
    /// A `.bk2` input log names its columns but holds no frame lines.
    Bk2NoFrames,
}

impl std::fmt::Display for MovieError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MovieError::BadLine(line) => write!(f, "line {line} is malformed"),
            MovieError::PowerCycleUnsupported => {
                write!(
                    f,
                    "the movie powers the console off and on, which needs a fresh machine"
                )
            }
            MovieError::Bk2Archive(message) => {
                write!(f, "the .bk2 archive could not be read: {message}")
            }
            MovieError::Bk2NoInputLog => {
                write!(f, "the .bk2 archive has no {BK2_INPUT_LOG:?} entry")
            }
            MovieError::Bk2Savestate => write!(
                f,
                "the .bk2 archive starts from a BizHawk savestate ({BK2_SAVESTATE}), which this \
                 build cannot load: play it in BizHawk, or record it from power-on instead"
            ),
            MovieError::Bk2NoLogKey => {
                write!(
                    f,
                    "the .bk2 input log has no LogKey: line, so its columns are unnamed"
                )
            }
            MovieError::Bk2NoFrames => write!(f, "the .bk2 input log has no frame lines"),
        }
    }
}

/// FM2 format version this build writes: the spec says "for now it is always 3".
pub const FM2_VERSION: i32 = 3;

/// FM2 `emuVersion`: the version of the emulator that produced the movie.
/// This crate has no release numbering, so recordings carry a fixed value.
pub const EMU_VERSION: i32 = 1;

/// The FM2 header key that counts how many times the recording was rewound
/// (FCEUX writes it for rerecording); also the gist of the `guid` key, which
/// ties a save state to the recording it was taken during.
pub const RERECORD_COUNT_KEY: &str = "rerecordCount";

/// The header key naming the recording; a save state remembers it so loading
/// that state cannot rewind a *different* recording.
pub const GUID_KEY: &str = "guid";

/// This build's own movie version: the schema of the `nesrust_` header keys, which FM2 has
/// no field for (`version` is always 3 by the spec). Version 2 adds [`RESET_START_KEY`]; a header
/// without the key was written before it existed, and [`Movie::movie_version`] reads it as 0.
pub const MOVIE_VERSION_KEY: &str = "nesrust_movie_version";

/// The [`MOVIE_VERSION_KEY`] this build writes.
pub const MOVIE_VERSION: i32 = 2;

/// How many Reset-button presses the machine had already taken when the recording began.
///
/// Without it a movie recorded mid-run numbered its own Resets from 0, so the first one replayed
/// at press 0's phase — the wrong phase under `--reset-align random`, and a desync. The player
/// seeds its press count from this, so the movie's first Reset is `start + 0`.
pub const RESET_START_KEY: &str = "nesrust_reset_start";

/// The canonical FM2 pad-order letters: position `i` of `RLDUTSBA`.
const FM2_PAD_CHARS: [char; 8] = ['R', 'L', 'D', 'U', 'T', 'S', 'B', 'A'];

/// FM2 position `i` maps to bit `7 - i`: `R` = `BUTTON_RIGHT` = bit 7, ..., `A` = bit 0.
fn fm2_position_bit(i: usize) -> u8 {
    7 - i as u8
}

fn pad_is_pressed(c: char) -> bool {
    c != '.' && c != ' '
}

/// Encode one pad mask as the 8-character FM2 field, canonical letters for set bits.
fn mask_to_fm2(mask: u8) -> String {
    let mut s = String::with_capacity(8);
    for (i, ch) in FM2_PAD_CHARS.iter().enumerate() {
        if mask & (1 << fm2_position_bit(i)) != 0 {
            s.push(*ch);
        } else {
            s.push('.');
        }
    }
    s
}

/// Parse one 8-character FM2 pad field into a joypad mask.
fn fm2_to_pad(field: &str) -> Option<u8> {
    if field.is_empty() {
        return Some(0); // port not in use: FM2 leaves the field empty
    }
    if field.len() != 8 {
        return None;
    }
    let mut mask = 0u8;
    for (i, c) in field.char_indices() {
        if pad_is_pressed(c) {
            mask |= 1 << fm2_position_bit(i);
        }
    }
    Some(mask)
}

fn is_frame_line(line: &str) -> bool {
    line.starts_with('|')
}

fn parse_frame_line(line: &str) -> Option<MovieFrame> {
    // Shape: "|c|pad1|pad2||" — split on '|' gives ["", c, pad1, pad2, "", ""].
    let parts: Vec<&str> = line.split('|').collect();
    if parts.len() != 6 || !parts[0].is_empty() || !parts[4].is_empty() || !parts[5].is_empty() {
        return None;
    }
    if parts[1].is_empty() || !parts[1].bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let commands: u8 = parts[1].parse().ok()?;
    let pad1 = fm2_to_pad(parts[2])?;
    let pad2 = fm2_to_pad(parts[3])?;
    Some(MovieFrame {
        commands,
        pads: [pad1, pad2],
    })
}

/// Parse an FM2 text file. Header lines are `key value` (split at the first
/// space); frame lines are `|c|RLDUTSBA|RLDUTSBA||`. Blank lines are ignored.
/// `MovieError::BadLine` carries the 1-based line number of the offending line.
pub fn parse_fm2(text: &str) -> Result<Movie, MovieError> {
    let mut movie = Movie::default();
    for (lineno, raw) in text.lines().enumerate() {
        let line = raw.trim_end_matches('\r');
        if line.is_empty() {
            continue;
        }
        if is_frame_line(line) {
            let frame = parse_frame_line(line).ok_or(MovieError::BadLine(lineno + 1))?;
            movie.frames.push(frame);
        } else {
            let (key, value) = line
                .split_once(' ')
                .ok_or(MovieError::BadLine(lineno + 1))?;
            movie.header.push((key.to_string(), value.to_string()));
        }
    }
    Ok(movie)
}

/// The archive entry a `.bk2` keeps its per-frame input in.
pub const BK2_INPUT_LOG: &str = "Input Log.txt";

/// The savestate a `.bk2` starts from, as its message names it.
const BK2_SAVESTATE: &str = "Core.bin";

/// Every entry name that anchors a `.bk2` to a savestate, lowercased: `Core.bin` from the
/// archives that carried one next to the log, `CoreState`/`CoreState.bin` from current
/// BizHawk. TASVideos is explicit that the state *file's* existence, not the header's
/// `StartsFromSavestate` flag, is what makes a movie savestate-anchored, so the archive's
/// entries are what is checked.
const BK2_SAVESTATES: [&str; 3] = ["core.bin", "corestate", "corestate.bin"];

/// Read the BizHawk `.bk2` archive at `path` into the same [`Movie`] an FM2 parses into.
///
/// Power-on is the only start this build can make, so an archive that carries the savestate its
/// first frame continues from is refused ([`MovieError::Bk2Savestate`]) instead of being played
/// from the game's frame 0: a wrong replay that looks right is worse than a named error.
pub fn parse_bk2(path: &Path) -> Result<Movie, MovieError> {
    let bytes = std::fs::read(path).map_err(|e| MovieError::Bk2Archive(e.to_string()))?;
    parse_bk2_bytes(&bytes)
}

/// [`parse_bk2`] over an archive already in memory, which is how the tests build one.
///
/// The entries are looked up by their own name, lowercased, so an archive that keeps them in a
/// folder (`nes/Input Log.txt`) or capitalises them differently still reads.
pub fn parse_bk2_bytes(bytes: &[u8]) -> Result<Movie, MovieError> {
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes))
        .map_err(|e| MovieError::Bk2Archive(e.to_string()))?;
    let mut input_index = None;
    let mut header_index = None;
    for index in 0..archive.len() {
        let entry = archive
            .by_index(index)
            .map_err(|e| MovieError::Bk2Archive(e.to_string()))?;
        if entry.is_dir() {
            continue;
        }
        let name = archive_entry_name(entry.name());
        if BK2_SAVESTATES.contains(&name.as_str()) {
            return Err(MovieError::Bk2Savestate);
        }
        // Two of the documented sample archives drop the `.txt` from both names, so the stem
        // is accepted too; anything else in the archive (comments, subtitles, sync settings) is
        // not input and is left alone.
        match name.as_str() {
            "input log.txt" | "input log" => input_index = Some(index),
            "header.txt" | "header" => header_index = Some(index),
            _ => {}
        }
    }
    let Some(input_index) = input_index else {
        return Err(MovieError::Bk2NoInputLog);
    };
    let mut movie = parse_bk2_log(&read_zip_text(&mut archive, input_index)?)?;
    if let Some(header_index) = header_index {
        movie.header = parse_bk2_header(&read_zip_text(&mut archive, header_index)?);
    }
    Ok(movie)
}

/// An archive entry's own name, lowercased: the folder it sits in is not part of its name, and
/// the capitalisation BizHawk writes is not something to depend on.
fn archive_entry_name(name: &str) -> String {
    name.rsplit('/').next().unwrap_or(name).to_ascii_lowercase()
}

/// Read one archive entry as text; BizHawk writes its text entries in UTF-8.
fn read_zip_text<R: Read + std::io::Seek>(
    archive: &mut zip::ZipArchive<R>,
    index: usize,
) -> Result<String, MovieError> {
    let mut entry = archive
        .by_index(index)
        .map_err(|e| MovieError::Bk2Archive(e.to_string()))?;
    let name = entry.name().to_owned();
    let mut bytes = Vec::new();
    entry
        .read_to_end(&mut bytes)
        .map_err(|e| MovieError::Bk2Archive(format!("{name}: {e}")))?;
    String::from_utf8(bytes).map_err(|_| MovieError::Bk2Archive(format!("{name} is not UTF-8")))
}

/// Parse a `.bk2`'s `Input Log.txt` into a [`Movie`]: the `LogKey:` line names the columns and
/// each `|`-prefixed line after it is one frame. `[Input]` and any other line outside a frame
/// is skipped, so the section markers BizHawk writes are not read as input.
///
/// The names are what decide the mapping, not the position: BizHawk's cores disagree on both
/// the order of the columns and whether the console's own buttons come first.
pub fn parse_bk2_log(text: &str) -> Result<Movie, MovieError> {
    let mut movie = Movie::default();
    let mut names: Option<Vec<String>> = None;
    for (lineno, raw) in text.lines().enumerate() {
        let line = raw.trim_end();
        if let Some(key) = line.trim_start().strip_prefix("LogKey:") {
            names = Some(parse_bk2_log_key(key));
            continue;
        }
        if !line.starts_with('|') {
            continue;
        }
        let Some(names) = names.as_deref() else {
            return Err(MovieError::Bk2NoLogKey);
        };
        let columns = bk2_frame_columns(line);
        if columns.len() != names.len() {
            return Err(MovieError::BadLine(lineno + 1));
        }
        let mut frame = MovieFrame {
            commands: 0,
            pads: [0, 0],
        };
        for (name, pressed) in names.iter().zip(&columns) {
            if !*pressed {
                continue;
            }
            match bk2_input_target(name) {
                Some(Bk2Input::Button(port, bit)) => frame.pads[port] |= bit,
                Some(Bk2Input::Reset) => frame.commands |= 0b01,
                Some(Bk2Input::Power) => frame.commands |= 0b10,
                // A column this build does not drive — another controller's, or a core knob —
                // is skipped instead of guessed at.
                None => {}
            }
        }
        movie.frames.push(frame);
    }
    if names.is_none() {
        return Err(MovieError::Bk2NoLogKey);
    }
    if movie.frames.is_empty() {
        return Err(MovieError::Bk2NoFrames);
    }
    Ok(movie)
}

/// The column names of a `LogKey:` line: `|`-separated, its `#` group markers dropped and the
/// empty field after the trailing `|` ignored. The `#` only marks where one controller's group
/// starts; it carries no information a per-column name does not already have.
fn parse_bk2_log_key(line: &str) -> Vec<String> {
    line.split('|')
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(|name| name.trim_start_matches('#').to_owned())
        .collect()
}

/// One frame line's columns, as "pressed" flags in `LogKey` order.
///
/// TASVideos: pipes "can be present anywhere in the input log and serve as logical
/// separators", so the fields are flattened and re-read column by column. A column is one
/// character — pressed when it is not `.` — unless it is analog (see [`bk2_column_width`]),
/// which is reported as not pressed: no NES pad button is analog, and the value is input this
/// build does not consume.
fn bk2_frame_columns(line: &str) -> Vec<bool> {
    let mut columns = Vec::new();
    for field in line.split('|').filter(|field| !field.is_empty()) {
        let bytes = field.as_bytes();
        let mut at = 0;
        while at < bytes.len() {
            let width = bk2_column_width(&bytes[at..]);
            columns.push(width == 1 && bytes[at] != b'.');
            at += width;
        }
    }
    columns
}

/// How many bytes the column at the front of `bytes` takes: 6 for an analog column (`    0,`)
/// and 1 for a button's character. Reading the width from the text, instead of assuming every
/// column is one character, is what keeps a core's analog knob (QuickNes's `Reset Cycle`) from
/// shifting every button after it by five columns.
fn bk2_column_width(bytes: &[u8]) -> usize {
    let analog = bytes.len() >= 6
        && bytes[5] == b','
        && bytes[..5]
            .iter()
            .all(|b| b.is_ascii_digit() || *b == b' ' || *b == b'-');
    if analog { 6 } else { 1 }
}

/// Where one `LogKey` column goes in a [`MovieFrame`].
enum Bk2Input {
    /// A pad button: which of the two ports, and the joypad bit it sets.
    Button(usize, u8),
    /// The console's reset button: the frame's command bit 0, a soft reset.
    Reset,
    /// The console's power button: the frame's command bit 1, which the frontend answers by
    /// rebuilding the machine, exactly as BizHawk power-cycles for every frame the button is
    /// held.
    Power,
}

/// Map a BizHawk NES column name — `P1 A`, `P2 Up`, `Reset`, `Power` — onto our frame bits.
///
/// `None` covers everything this build does not drive: a FourScore's `P3`/`P4`, a core knob,
/// and any other system's names. Anything unmatched is skipped rather than guessed at, so a
/// name this function does not know cannot press the wrong button.
fn bk2_input_target(name: &str) -> Option<Bk2Input> {
    let mut words = name.split_whitespace();
    let head = words.next()?;
    // A one-word column is the console's own. A two-word one is a pad button — or a core knob
    // like QuickNes's `Reset Cycle`, which is `Reset` plus a word and is not a button.
    let Some(button) = words.next() else {
        return if head.eq_ignore_ascii_case("reset") {
            Some(Bk2Input::Reset)
        } else if head.eq_ignore_ascii_case("power") {
            Some(Bk2Input::Power)
        } else {
            None
        };
    };
    let port = match head.to_ascii_uppercase().as_str() {
        "P1" => 0,
        "P2" => 1,
        _ => return None,
    };
    let bit = match button.to_ascii_uppercase().as_str() {
        "UP" => BUTTON_UP,
        "DOWN" => BUTTON_DOWN,
        "LEFT" => BUTTON_LEFT,
        "RIGHT" => BUTTON_RIGHT,
        "START" => BUTTON_START,
        "SELECT" => BUTTON_SELECT,
        "B" => BUTTON_B,
        "A" => BUTTON_A,
        _ => return None,
    };
    Some(Bk2Input::Button(port, bit))
}

/// Read `Header.txt`'s `key value` lines into [`Movie::header`], so a `.bk2` and an FM2 carry
/// the same kind of header. BizHawk writes none of the `nesrust_` keys, so
/// [`console_from_movie_header`] finds no console there and a `.bk2` replays on the command
/// line's console, which is all this build knows about the machine it was recorded on.
fn parse_bk2_header(text: &str) -> Vec<(String, String)> {
    text.lines()
        .map(|line| line.trim_end_matches('\r'))
        .filter(|line| !line.is_empty() && !line.starts_with('['))
        .filter_map(|line| {
            line.split_once(' ')
                .map(|(key, value)| (key.to_owned(), value.trim().to_owned()))
        })
        .collect()
}

impl Movie {
    /// Render as FM2 text: header lines in order, then one line per frame,
    /// each terminated by `\n`. An unpressed pad is `........`.
    pub fn to_fm2(&self) -> String {
        let mut out = String::new();
        for (key, value) in &self.header {
            push_header_line(&mut out, key, value);
        }
        for frame in &self.frames {
            push_frame_line(&mut out, frame);
        }
        out
    }

    /// The header's [`MOVIE_VERSION_KEY`], or 0 when it has none — a movie recorded before the key
    /// existed (or by another tool, which writes none of the `nesrust_` keys).
    pub fn movie_version(&self) -> i32 {
        self.header
            .iter()
            .rev()
            .find(|(key, _)| key == MOVIE_VERSION_KEY)
            .and_then(|(_, value)| value.parse().ok())
            .unwrap_or(0)
    }

    /// The Reset-button press count the recording started at: the header's
    /// [`RESET_START_KEY`], or 0 when it names none or the value is not a number.
    ///
    /// A movie recorded at power-on, or one recorded before the key existed, starts at 0 — which
    /// is what it replayed with anyway, so an old file plays exactly as it always did. The key is
    /// read only from a header whose [`Movie::movie_version`] is [`MOVIE_VERSION`] or newer: a
    /// count without a version is not trusted to mean what this build means by it.
    pub fn reset_start(&self) -> u32 {
        if self.movie_version() < MOVIE_VERSION {
            return 0;
        }
        self.header
            .iter()
            .rev()
            .find(|(key, _)| key == RESET_START_KEY)
            .and_then(|(_, value)| value.parse().ok())
            .unwrap_or(0)
    }
}

fn push_header_line(out: &mut String, key: &str, value: &str) {
    out.push_str(key);
    out.push(' ');
    out.push_str(value);
    out.push('\n');
}

fn push_frame_line(out: &mut String, frame: &MovieFrame) {
    out.push('|');
    out.push_str(&frame.commands.to_string());
    out.push('|');
    out.push_str(&mask_to_fm2(frame.pads[0]));
    out.push('|');
    out.push_str(&mask_to_fm2(frame.pads[1]));
    out.push_str("||\n");
}

/// FM2 `romChecksum` for PRG and CHR ROM bytes: `base64:` followed by the base64 of the
/// 16-byte MD5 of `prg` then `chr` — the iNES image with its 16-byte header and any trainer
/// left out, which is the ROM data FCEUX hashes. Pass the bytes the cartridge actually holds;
/// [`rom_checksum_for`] applies that rule to a whole cartridge.
pub fn rom_checksum(prg: &[u8], chr: &[u8]) -> String {
    let mut data = Vec::with_capacity(prg.len() + chr.len());
    data.extend_from_slice(prg);
    data.extend_from_slice(chr);
    format!("base64:{}", checksum::base64(&checksum::md5(&data)))
}

/// FM2 `romChecksum` for a loaded cartridge: PRG bytes then CHR ROM bytes, and no CHR bytes at
/// all for a CHR-RAM board. `cart::split` fills CHR RAM in with an 8192-byte zero image, which is
/// not ROM data and must not be hashed — FCEUX hashes VROM only when the cartridge has it, so a
/// recording made on a CHR-RAM game matches the same ROM in FCEUX/BizHawk/Mesen.
pub fn rom_checksum_for(data: &RomData) -> String {
    let chr: &[u8] = if data.chr_is_ram { &[] } else { &data.chr };
    rom_checksum(&data.prg, chr)
}

/// A movie GUID in the spec's `XXXXXXXX-XXXX-XXXX-XXXX-XXXXXXXXXXXX` form,
/// derived from `seed` (the frontend passes the wall clock). Two xorshift64
/// rounds fill the 16 bytes so no random-number crate is needed, and a fixed
/// seed is reproducible, which is what tests want.
pub fn guid_from_seed(seed: u64) -> String {
    let mut x = seed ^ 0x9E37_79B9_7F4A_7C15;
    let mut bytes = [0u8; 16];
    for chunk in bytes.chunks_mut(8) {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        chunk.copy_from_slice(&x.to_be_bytes());
    }

    let mut out = String::with_capacity(36);
    for (i, byte) in bytes.iter().enumerate() {
        if matches!(i, 4 | 6 | 8 | 10) {
            out.push('-');
        }
        out.push_str(&format!("{byte:02X}"));
    }
    out
}

/// The header a recording of `rom_filename` starts with: the keys the spec marks required,
/// `version` first as the spec demands, then the device flags for two standard gamepads — no
/// FourScore, zapper, FDS or New PPU — and `palFlag` from the cartridge's `region`, because a
/// PAL or Dendy recording replayed at NTSC timing desyncs. The console is the plain default; a
/// recording made on another one uses [`fm2_header_console`].
pub fn fm2_header(
    rom_filename: &str,
    rom_checksum: &str,
    guid: &str,
    region: Region,
) -> Vec<(String, String)> {
    fm2_header_console(
        rom_filename,
        rom_checksum,
        guid,
        region,
        &ConsoleConfig::default(),
        0,
    )
}

/// [`fm2_header`] plus the console the recording ran on: the power-on options
/// (`--ram-init`, `--align`, `--reset-align`, `--random-mapper`, `--seed`) and the hardware quirks
/// (`--quirks`, `--console`), as `nesrust_`-prefixed keys.
///
/// FM2 allows unknown header keys, so other tools ignore them; this emulator reads them back with
/// [`console_from_movie_header`] and replays the recording on the console it was made on instead
/// of on whatever the command line happens to say.
///
/// `reset_start` is how many Reset-button presses the machine had already taken when the recording
/// began: the frontend passes its press count, and the player seeds its own from the key so
/// the recording's first Reset replays at the same press index. A recording that starts at
/// power-on passes 0.
pub fn fm2_header_console(
    rom_filename: &str,
    rom_checksum: &str,
    guid: &str,
    region: Region,
    console: &ConsoleConfig,
    reset_start: u32,
) -> Vec<(String, String)> {
    // FCEUX replays a Dendy cartridge at PAL timing, so both PAL regions set palFlag.
    let pal = if matches!(region, Region::Pal | Region::Dendy) {
        "1"
    } else {
        "0"
    };
    let mut header = vec![
        ("version".to_owned(), FM2_VERSION.to_string()),
        ("emuVersion".to_owned(), EMU_VERSION.to_string()),
        ("emuName".to_owned(), "nes-rust".to_owned()),
        ("romFilename".to_owned(), rom_filename.to_owned()),
        ("romChecksum".to_owned(), rom_checksum.to_owned()),
        ("guid".to_owned(), guid.to_owned()),
        // Two standard gamepads, no FourScore/FDS/New PPU, timing from the cartridge.
        ("palFlag".to_owned(), pal.to_owned()),
        ("fourscore".to_owned(), "0".to_owned()),
        ("port0".to_owned(), "1".to_owned()),
        ("port1".to_owned(), "1".to_owned()),
        ("port2".to_owned(), "0".to_owned()),
        ("NewPPU".to_owned(), "0".to_owned()),
        ("FDS".to_owned(), "0".to_owned()),
    ];
    header.extend(console.to_header());
    // this build's own movie version, then the press count the recording started at. An FM2
    // `version` stays 3 (the spec fixes it), so the `nesrust_` schema carries its own number.
    header.push((MOVIE_VERSION_KEY.to_owned(), MOVIE_VERSION.to_string()));
    header.push((RESET_START_KEY.to_owned(), reset_start.to_string()));
    header
}

/// One field where a recording's console and the replaying run's console disagree: the FM2 header
/// key, the value the movie records and the value the command line asked for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigDiff {
    pub key: String,
    pub movie: String,
    pub cli: String,
}

/// The console a movie's header names, and how it differs from `cli`.
///
/// `Ok(None)` when the header names none — a movie recorded before the keys existed, or by
/// another tool — and the caller keeps the console it already has, which is what those movies
/// have always replayed with.
///
/// When the header does name a console, that is the one to build: a recording only reproduces its
/// run on the machine it was made on, so the header wins over `cli` and every field where the two
/// disagree is returned for the caller to report — the run the user asked for on the command line
/// is not the run that plays, and silently ignoring either side would hide a desync.
pub fn console_from_movie_header(
    header: &[(String, String)],
    cli: &ConsoleConfig,
) -> Result<Option<(ConsoleConfig, Vec<ConfigDiff>)>, String> {
    let Some(movie) = ConsoleConfig::from_header(header)? else {
        return Ok(None);
    };
    let cli_header = cli.to_header();
    let diffs = movie
        .to_header()
        .into_iter()
        .filter_map(|(key, movie)| {
            let cli = cli_header
                .iter()
                .find(|(cli_key, _)| *cli_key == key)
                .map(|(_, value)| value.clone())?;
            (movie != cli).then_some(ConfigDiff { key, movie, cli })
        })
        .collect();
    Ok(Some((movie, diffs)))
}

/// Play an FM2 movie onto a live `Nes`: call
/// [`Player::apply`](Player::apply) before every `run_frame`.
pub struct Player {
    pub movie: Movie,
    /// Index of the next frame to apply.
    pub pos: usize,
    /// The console's power-on options, for the one a replay has to reproduce: the reset
    /// alignment says how far each recorded Reset moves the PPU, so a reset frame replays as the
    /// same realigned reset the recorder performed instead of a plain phase-0
    /// [`Nes::reset`]. [`Player::new`] leaves it at [`PowerOn::default()`], whose reset alignment
    /// is phase 0 — the plain reset this player always did.
    pub power_on: PowerOn,
    /// Reset-button presses applied since power-on, zero-based: the index
    /// [`PowerOn::reset_phase_for`] folds into the seed stream. A movie power cycle rebuilds the
    /// machine, which is a fresh power-on, so the count starts over at 0 there.
    reset_index: u32,
}

impl Player {
    /// A player for a movie on a console with the default power-on options: every recorded Reset
    /// is the plain phase-0 reset.
    pub fn new(movie: Movie) -> Self {
        Player::with_power_on(movie, PowerOn::default())
    }

    /// A player that replays each recorded Reset with the console's own reset alignment:
    /// `power_on` is the one the machine was built with — its `reset_phase` and `seed`, which a
    /// recording writes as the `nesrust_reset_align`/`nesrust_seed` header keys — so the
    /// `n`-th Reset of the movie applies `power_on.reset_phase_for(n)` dots, exactly as the
    /// recorder's R key did for that press.
    ///
    /// `n` counts Reset presses since power-on, not Resets within the movie: it starts at the
    /// [`RESET_START_KEY`] count the recording wrote for where it began, so a recording
    /// started mid-run after earlier presses replays its first Reset at that press's phase. A
    /// movie recorded at power-on, or an old one without the key, starts at 0, which is what it
    /// replayed with before.
    pub fn with_power_on(movie: Movie, power_on: PowerOn) -> Self {
        let reset_index = movie.reset_start();
        Player {
            movie,
            pos: 0,
            power_on,
            reset_index,
        }
    }

    /// Before `nes.run_frame()`: set both pads from the next frame and, if the frame's command
    /// bit 0 is set, press Reset with the console's reset alignment ([`Player::with_power_on`]).
    /// Returns `Ok(false)` when the movie has ended. Command bit 1 (power cycle) is reported as
    /// [`MovieError::PowerCycleUnsupported`] without consuming the frame: a power cycle needs a
    /// fresh machine, which only the frontend can build, so it rebuilds and calls
    /// [`Player::apply_power_cycle`] before applying the frame again.
    pub fn apply(&mut self, nes: &mut Nes) -> Result<bool, MovieError> {
        let Some(frame) = self.movie.frames.get(self.pos) else {
            return Ok(false);
        };
        if frame.commands & 0b10 != 0 {
            return Err(MovieError::PowerCycleUnsupported);
        }
        nes.bus.pads[0].buttons = frame.pads[0];
        nes.bus.pads[1].buttons = frame.pads[1];
        if frame.commands & 0b01 != 0 {
            nes.reset_aligned(self.power_on.reset_phase_for(self.reset_index));
            self.reset_index = self.reset_index.saturating_add(1);
        }
        self.pos += 1;
        Ok(true)
    }

    /// Clear the power-cycle bit of the frame at the current position, after the frontend has
    /// rebuilt the machine from the ROM. The frame's other commands (a soft reset) still apply
    /// when it is played; the position does not move. The rebuild is a fresh power-on, so the
    /// reset-button count starts over: the movie's next Reset is that machine's first.
    pub fn apply_power_cycle(&mut self) {
        if let Some(frame) = self.movie.frames.get_mut(self.pos) {
            frame.commands &= !0b10;
        }
        self.reset_index = 0;
    }
}

/// Where a save state sits in a recording: the movie frame index the machine
/// state corresponds to, plus the GUID of the recording it was taken during.
///
/// Kept in a sidecar next to the state file (the frontend writes it as
/// `<rom>.state.anchor`) instead of inside the state, so the machine-state
/// format and its version do not change. Loading such a state while that same
/// recording is still running rewinds the movie to `frame`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StateAnchor {
    /// Index of the next frame the recording would append after the state.
    pub frame: usize,
    /// The recording's `guid` header value; an anchor only ever rewinds the
    /// recording it was taken during.
    pub guid: String,
}

impl StateAnchor {
    /// Render as the sidecar's text: one `key value` line per field.
    pub fn to_text(&self) -> String {
        format!("frame {}\n{} {}\n", self.frame, GUID_KEY, self.guid)
    }

    /// Parse sidecar text; `None` when it is not an anchor this build wrote.
    pub fn parse(text: &str) -> Option<StateAnchor> {
        let mut frame = None;
        let mut guid = None;
        for line in text.lines() {
            let (key, value) = line.split_once(' ')?;
            match key {
                "frame" => frame = Some(value.parse().ok()?),
                GUID_KEY => guid = Some(value.to_owned()),
                _ => {}
            }
        }
        Some(StateAnchor {
            frame: frame?,
            guid: guid?,
        })
    }
}

/// Record an FM2 movie from a live `Nes`: call
/// [`Recorder::record`](Recorder::record) after setting the pads and before
/// every `run_frame`.
pub struct Recorder {
    pub movie: Movie,
}

impl Recorder {
    pub fn new(header: Vec<(String, String)>) -> Self {
        Recorder {
            movie: Movie {
                header,
                frames: Vec::new(),
            },
        }
    }

    /// After setting the pads and before `run_frame`: append the current pads, and the Reset
    /// command bit when the frontend says a Reset press applies to this frame.
    pub fn record(&mut self, nes: &Nes, reset_pressed: bool) {
        self.movie.frames.push(MovieFrame {
            commands: u8::from(reset_pressed),
            pads: [nes.bus.pads[0].buttons, nes.bus.pads[1].buttons],
        });
    }

    /// The recording's own `guid`, which a save state's anchor names.
    pub fn guid(&self) -> Option<&str> {
        self.movie
            .header
            .iter()
            .find(|(key, _)| key == GUID_KEY)
            .map(|(_, value)| value.as_str())
    }

    /// How many times the recording has been rewound: the header's
    /// [`RERECORD_COUNT_KEY`], or 0 when it is absent or not a number (a movie
    /// from another tool that never rerecorded).
    pub fn rerecord_count(&self) -> u32 {
        self.movie
            .header
            .iter()
            .find(|(key, _)| key == RERECORD_COUNT_KEY)
            .and_then(|(_, value)| value.parse().ok())
            .unwrap_or(0)
    }

    fn set_rerecord_count(&mut self, count: u32) {
        let value = count.to_string();
        match self
            .movie
            .header
            .iter_mut()
            .find(|(key, _)| key == RERECORD_COUNT_KEY)
        {
            Some(pair) => pair.1 = value,
            None => self
                .movie
                .header
                .push((RERECORD_COUNT_KEY.to_owned(), value)),
        }
    }

    /// Rewind the recording to `frame`: drop every frame recorded after it so
    /// recording can continue from a state taken there, and count the rerecord
    /// in the header. Returns how many frames were dropped. A `frame` at or
    /// past the end drops nothing and does not count as a rerecord.
    pub fn truncate_to(&mut self, frame: usize) -> usize {
        let keep = frame.min(self.movie.frames.len());
        let dropped = self.movie.frames.len() - keep;
        if dropped == 0 {
            return 0;
        }
        self.movie.frames.truncate(keep);
        let count = self.rerecord_count() + 1;
        self.set_rerecord_count(count);
        dropped
    }

    /// The anchor for a save state taken now, at the end of the recording.
    /// `None` when the header has no `guid`: without one a state cannot be
    /// tied to this recording, so it cannot rewind it either.
    pub fn anchor(&self) -> Option<StateAnchor> {
        Some(StateAnchor {
            frame: self.movie.frames.len(),
            guid: self.guid()?.to_owned(),
        })
    }

    /// Rewind to the frame `anchor` names — FCEUX-style rerecording — but only
    /// when the anchor belongs to this recording. Returns the frames dropped.
    pub fn truncate_to_anchor(&mut self, anchor: &StateAnchor) -> usize {
        if self.guid() != Some(anchor.guid.as_str()) {
            return 0;
        }
        self.truncate_to(anchor.frame)
    }

    /// Render the recording for saving: the movie's header with
    /// [`RERECORD_COUNT_KEY`] always present (0 when never rewound), then the
    /// frames.
    pub fn to_fm2(&self) -> String {
        let mut out = String::new();
        for (key, value) in &self.movie.header {
            push_header_line(&mut out, key, value);
        }
        if self
            .movie
            .header
            .iter()
            .all(|(key, _)| key != RERECORD_COUNT_KEY)
        {
            push_header_line(
                &mut out,
                RERECORD_COUNT_KEY,
                &self.rerecord_count().to_string(),
            );
        }
        for frame in &self.movie.frames {
            push_frame_line(&mut out, frame);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::io::Write;

    use crate::bus::RamPowerOn;
    use crate::console::{ConsoleConfig, Quirks};
    use crate::controller::{
        BUTTON_A, BUTTON_B, BUTTON_LEFT, BUTTON_RIGHT, BUTTON_START, BUTTON_UP,
    };
    use crate::nes::{PhaseChoice, PowerOn};

    #[test]
    fn parse_header_and_pressed_buttons() {
        let movie = parse_fm2("version 3\n|0|R......A|........||\n").expect("parse should succeed");
        assert_eq!(movie.header, vec![("version".to_string(), "3".to_string())]);
        assert_eq!(movie.frames.len(), 1);
        assert_eq!(movie.frames[0].commands, 0);
        assert_eq!(movie.frames[0].pads[0], BUTTON_RIGHT | BUTTON_A);
        assert_eq!(movie.frames[0].pads[1], 0);
    }

    #[test]
    fn parse_reset_command_and_up_bit() {
        let movie = parse_fm2("|1|........|...U....||").expect("parse should succeed");
        assert_eq!(movie.frames.len(), 1);
        assert_eq!(movie.frames[0].commands, 1);
        assert_eq!(movie.frames[0].pads[0], 0);
        assert_eq!(movie.frames[0].pads[1], BUTTON_UP);
    }

    #[test]
    fn bad_line_reports_1_based_line_number() {
        let err = parse_fm2("|x|").expect_err("a frame with a non-digit command is bad");
        assert_eq!(err, MovieError::BadLine(1));
    }

    #[test]
    fn bad_line_counts_header_lines() {
        let err = parse_fm2("version 3\nplane 7\n|x|").expect_err("bad frame on text line 3");
        assert_eq!(err, MovieError::BadLine(3));
    }

    #[test]
    fn bad_pad_width_is_reported() {
        let err = parse_fm2("|0|R......||").expect_err("pad field of 7 chars is bad");
        assert_eq!(err, MovieError::BadLine(1));
    }

    #[test]
    fn empty_unused_port_field_is_zero() {
        // Real FM2 files leave unused ports empty: port0 only is `|0|R.......|||`.
        let movie = parse_fm2("|0|R.......|||\n").expect("parse should succeed");
        assert_eq!(movie.frames[0].pads, [BUTTON_RIGHT, 0]);
    }

    #[test]
    fn space_means_pressed_like_dot_is_open() {
        // A space at a position is not pressed; any other character is.
        let movie = parse_fm2("|0|  ......|........||").expect("parse should succeed");
        assert_eq!(movie.frames[0].pads[0], 0);
        let movie = parse_fm2("|0|R......X|........||").expect("parse should succeed");
        assert_eq!(movie.frames[0].pads[0], BUTTON_RIGHT | BUTTON_A);
    }

    #[test]
    fn fm2_round_trip_three_frames() {
        let sample = "version 3\nemuName somenes\n\
                      |0|RLDU.S.A|........||\n|1|........|...U....||\n|3|R......A|R.DU....||\n";
        let movie = parse_fm2(sample).expect("parse should succeed");
        assert_eq!(movie.frames.len(), 3);
        assert_eq!(movie.frames[2].commands, 3);
        assert_eq!(movie.to_fm2(), sample);
    }

    #[test]
    fn empty_pad_renders_as_dots() {
        let mut movie = Movie::default();
        movie.frames.push(MovieFrame {
            commands: 0,
            pads: [0, 0],
        });
        assert_eq!(movie.to_fm2(), "|0|........|........||\n");
    }

    #[test]
    fn rom_checksum_is_base64_of_the_md5_of_prg_then_chr() {
        // Known vectors, hashed independently: md5("") and md5 of 64 bytes
        // 0x00..0x3F followed by AA BB CC.
        assert_eq!(rom_checksum(&[], &[]), "base64:1B2M2Y8AsgTpgAmY7PhCfg==");
        let prg: Vec<u8> = (0..64).collect();
        assert_eq!(
            rom_checksum(&prg, &[0xAA, 0xBB, 0xCC]),
            "base64:QtOj+/BkWfLeALmz4kIkNA=="
        );
        // PRG comes before CHR, so swapping two different halves changes the hash.
        assert_ne!(rom_checksum(&[1, 2], &[3]), rom_checksum(&[3], &[1, 2]));
    }

    #[test]
    fn guid_has_the_spec_shape_and_follows_its_seed() {
        let guid = guid_from_seed(0x0123_4567_89AB_CDEF);
        assert_eq!(guid.len(), 36);
        let dashes: Vec<usize> = guid
            .char_indices()
            .filter(|(_, c)| *c == '-')
            .map(|(at, _)| at)
            .collect();
        assert_eq!(dashes, vec![8, 13, 18, 23]);
        // FCEUX writes GUIDs in uppercase hex.
        assert!(
            guid.chars()
                .all(|c| c == '-' || c.is_ascii_digit() || c.is_ascii_uppercase())
        );
        assert_eq!(guid, guid_from_seed(0x0123_4567_89AB_CDEF));
        assert_ne!(guid, guid_from_seed(0x0123_4567_89AB_CDEE));
        assert_ne!(guid_from_seed(0), "00000000-0000-0000-0000-000000000000");
    }

    #[test]
    fn header_has_every_required_key() {
        let guid = "01234567-89AB-CDEF-0123-456789ABCDEF";
        let header = fm2_header(
            "zelda.nes",
            "base64:1B2M2Y8AsgTpgAmY7PhCfg==",
            guid,
            Region::Ntsc,
        );
        // The spec: "the first key must be version", and the version is always 3.
        assert_eq!(header[0], ("version".to_owned(), "3".to_owned()));
        let value = |key: &str| {
            header
                .iter()
                .find(|(k, _)| k == key)
                .map_or_else(|| panic!("missing {key}"), |(_, v)| v.clone())
        };
        assert!(
            value("emuVersion").parse::<i32>().is_ok(),
            "emuVersion is an int32"
        );
        assert_eq!(value("romFilename"), "zelda.nes");
        assert_eq!(value("romChecksum"), "base64:1B2M2Y8AsgTpgAmY7PhCfg==");
        assert_eq!(value("guid"), guid);
        assert_eq!(value("palFlag"), "0");
        assert_eq!(value("fourscore"), "0");
        assert_eq!(value("port0"), "1");
        assert_eq!(value("port1"), "1");
        assert_eq!(value("port2"), "0");
        assert_eq!(value("NewPPU"), "0");
        assert_eq!(value("FDS"), "0");
    }

    #[test]
    fn header_sets_pal_flag_from_region() {
        let flag = |region: Region| {
            fm2_header(
                "x.nes",
                "base64:1B2M2Y8AsgTpgAmY7PhCfg==",
                "01234567-89AB-CDEF-0123-456789ABCDEF",
                region,
            )
            .into_iter()
            .find(|(k, _)| k == "palFlag")
            .map(|(_, v)| v)
            .expect("palFlag is a required key")
        };
        assert_eq!(flag(Region::Ntsc), "0");
        // A Dendy cartridge is replayed at PAL timing, so it gets palFlag 1 as well.
        assert_eq!(flag(Region::Pal), "1");
        assert_eq!(flag(Region::Dendy), "1");
    }

    #[test]
    fn recorded_header_round_trips_through_parse_and_to_fm2() {
        let header = fm2_header(
            "zelda.nes",
            &rom_checksum(b"prg", b"chr"),
            &guid_from_seed(42),
            Region::Ntsc,
        );
        let mut recorder = Recorder::new(header.clone());
        recorder.movie.frames.push(MovieFrame {
            commands: 1,
            pads: [BUTTON_A, BUTTON_UP],
        });
        let text = recorder.movie.to_fm2();
        let parsed = parse_fm2(&text).expect("a recorded movie parses");
        assert_eq!(parsed.header, header);
        assert_eq!(parsed.frames, recorder.movie.frames);
        assert_eq!(parsed.to_fm2(), text);
    }

    fn five_frames() -> Recorder {
        let mut recorder = Recorder::new(vec![(GUID_KEY.to_owned(), "G1".to_owned())]);
        for _ in 0..5 {
            recorder.movie.frames.push(MovieFrame {
                commands: 0,
                pads: [BUTTON_A, 0],
            });
        }
        recorder
    }

    #[test]
    fn truncate_to_drops_the_tail_and_counts_the_rerecord() {
        let mut recorder = five_frames();
        assert_eq!(recorder.truncate_to(2), 3);
        assert_eq!(recorder.movie.frames.len(), 2);
        assert_eq!(recorder.rerecord_count(), 1);

        // The count is written when the recording is saved, and a parse keeps it.
        let text = recorder.to_fm2();
        assert!(text.contains("rerecordCount 1\n"), "{text}");
        let parsed = parse_fm2(&text).expect("a saved recording parses");
        assert_eq!(parsed.frames.len(), 2);
        let count = parsed.header.iter().find(|(k, _)| k == RERECORD_COUNT_KEY);
        assert_eq!(count.map(|(_, v)| v.as_str()), Some("1"));
    }

    #[test]
    fn truncating_at_or_past_the_end_is_not_a_rerecord() {
        let mut recorder = five_frames();
        assert_eq!(recorder.truncate_to(5), 0);
        assert_eq!(recorder.truncate_to(99), 0);
        assert_eq!(recorder.movie.frames.len(), 5);
        assert_eq!(recorder.rerecord_count(), 0);
    }

    #[test]
    fn saving_a_recording_always_writes_the_rerecord_count() {
        let mut recorder = Recorder::new(vec![("version".to_owned(), "3".to_owned())]);
        recorder.movie.frames.push(MovieFrame {
            commands: 0,
            pads: [BUTTON_A, 0],
        });
        assert!(recorder.to_fm2().contains("rerecordCount 0\n"));
        // Only the saved text gains the key; the movie's header is untouched.
        assert_eq!(recorder.movie.header.len(), 1);
        // A foreign movie's count that is not a number reads as none.
        let foreign = Recorder::new(vec![(RERECORD_COUNT_KEY.to_owned(), "many".to_owned())]);
        assert_eq!(foreign.rerecord_count(), 0);
    }

    #[test]
    fn state_anchor_round_trips_through_text() {
        let anchor = StateAnchor {
            frame: 42,
            guid: guid_from_seed(9),
        };
        assert_eq!(StateAnchor::parse(&anchor.to_text()), Some(anchor));
        // Neither field alone, nor unrelated text, is an anchor.
        assert_eq!(StateAnchor::parse("frame 42\n"), None);
        assert_eq!(StateAnchor::parse("frame 42\nother 7\n"), None);
        assert_eq!(StateAnchor::parse("junk\n"), None);
    }

    #[test]
    fn anchor_rewinds_only_the_recording_it_was_taken_during() {
        let mut recorder = five_frames();
        let anchor = recorder
            .anchor()
            .expect("a recording with a guid has an anchor");
        assert_eq!(anchor.frame, 5);

        // Another recording's anchor leaves this one alone.
        let other = StateAnchor {
            frame: 1,
            guid: "THEIRS".to_owned(),
        };
        assert_eq!(recorder.truncate_to_anchor(&other), 0);
        assert_eq!(recorder.movie.frames.len(), 5);
        assert_eq!(recorder.rerecord_count(), 0);

        // Its own anchor drops what was recorded after it, and counts once.
        recorder.movie.frames.push(MovieFrame {
            commands: 0,
            pads: [BUTTON_UP, 0],
        });
        recorder.movie.frames.push(MovieFrame {
            commands: 0,
            pads: [BUTTON_UP, 0],
        });
        assert_eq!(recorder.truncate_to_anchor(&anchor), 2);
        assert_eq!(recorder.movie.frames.len(), 5);
        assert_eq!(recorder.rerecord_count(), 1);
    }

    #[test]
    fn a_recording_without_a_guid_has_no_anchor() {
        let recorder = Recorder::new(vec![("version".to_owned(), "3".to_owned())]);
        assert_eq!(recorder.guid(), None);
        assert_eq!(recorder.anchor(), None);
    }

    // -----------------------------------------------------------------------------------------
    // The console in the movie header.
    // -----------------------------------------------------------------------------------------

    #[test]
    fn a_recording_writes_the_console_it_ran_on() {
        // The default console is written explicitly, so a movie says what it ran on instead of
        // saying nothing (which would mean "whatever the command line picks").
        let header = fm2_header(
            "zelda.nes",
            &rom_checksum(b"prg", b"chr"),
            &guid_from_seed(1),
            Region::Ntsc,
        );
        let value = |key: &str| {
            header
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.as_str())
                .unwrap_or_else(|| panic!("missing {key}"))
        };
        assert_eq!(value("nesrust_ram_init"), "zero");
        assert_eq!(value("nesrust_align"), "0");
        assert_eq!(value("nesrust_reset_align"), "0");
        assert_eq!(value("nesrust_random_mapper"), "0");
        assert_eq!(value("nesrust_seed"), "0");
        assert_eq!(value("nesrust_overclock"), "0");
        assert_eq!(value("nesrust_quirks"), "");
        // The keys survive a save/parse, and read back as the plain default console.
        let text = Recorder::new(header).movie.to_fm2();
        let parsed = parse_fm2(&text).expect("a header-only movie parses");
        assert_eq!(
            ConsoleConfig::from_header(&parsed.header),
            Ok(Some(ConsoleConfig::default()))
        );
    }

    #[test]
    fn a_recording_writes_its_movie_version_and_the_reset_count_it_started_at() {
        // an FM2 `version` stays 3, so the `nesrust_` schema carries its own version, and the
        // press count the recording began at rides next to it.
        let header = fm2_header_console(
            "zelda.nes",
            &rom_checksum(b"prg", b"chr"),
            &guid_from_seed(1),
            Region::Ntsc,
            &ConsoleConfig::default(),
            2,
        );
        let text = Recorder::new(header).movie.to_fm2();
        let parsed = parse_fm2(&text).expect("a header-only movie parses");
        assert_eq!(parsed.movie_version(), MOVIE_VERSION);
        assert_eq!(parsed.reset_start(), 2);
        assert_eq!(
            parsed.header[0],
            ("version".to_owned(), "3".to_owned()),
            "FM2's own version"
        );
    }

    #[test]
    fn a_movie_from_before_the_reset_count_starts_at_press_zero() {
        // The old file: no `nesrust_movie_version`, no `nesrust_reset_start`. It reads as version 0
        // and starts at press 0 — the numbering it always had, so it still plays as it always did.
        let text = "version 3\nemuName somenes\n|1|........|........||\n";
        let parsed = parse_fm2(text).expect("parses");
        assert_eq!(parsed.movie_version(), 0);
        assert_eq!(parsed.reset_start(), 0);
        let player = Player::new(parsed);
        assert_eq!(player.reset_index, 0);
    }

    #[test]
    fn a_count_without_a_version_is_not_trusted() {
        // A header that names a count but no version is not one this build wrote: read 0 rather
        // than a number whose meaning is unknown.
        let mut movie = Movie::default();
        movie
            .header
            .push((RESET_START_KEY.to_owned(), "2".to_owned()));
        assert_eq!(movie.reset_start(), 0);
        movie
            .header
            .push((MOVIE_VERSION_KEY.to_owned(), MOVIE_VERSION.to_string()));
        assert_eq!(movie.reset_start(), 2);
    }

    #[test]
    fn the_movie_console_is_the_one_that_replays() {
        let config = ConsoleConfig::with_quirks(
            PowerOn {
                ram: RamPowerOn::Random,
                phase: PhaseChoice::Random,
                reset_phase: PhaseChoice::Random,
                random_mapper: true,
                seed: 7,
                seed_auto: false,
                overclock: 0,
            },
            Quirks {
                ppu_warmup: true,
                oam_decay: true,
                ..Quirks::default()
            },
        );
        let header = fm2_header_console(
            "zelda.nes",
            &rom_checksum(b"prg", b"chr"),
            &guid_from_seed(1),
            Region::Ntsc,
            &config,
            0,
        );
        let text = Recorder::new(header).movie.to_fm2();
        let parsed = parse_fm2(&text).expect("a header-only movie parses");

        // Replaying with no flags at all: the header's console wins, and every field where the
        // command line's default disagrees is reported.
        let (movie, diffs) = console_from_movie_header(&parsed.header, &ConsoleConfig::default())
            .expect("a recorded header reads back")
            .expect("the header names a console");
        assert_eq!(movie, config);
        let keys: Vec<&str> = diffs.iter().map(|d| d.key.as_str()).collect();
        assert_eq!(
            keys,
            vec![
                "nesrust_ram_init",
                "nesrust_align",
                "nesrust_reset_align",
                "nesrust_random_mapper",
                "nesrust_seed",
                "nesrust_quirks"
            ]
        );
        let ram = &diffs[0];
        assert_eq!((ram.movie.as_str(), ram.cli.as_str()), ("random", "zero"));
        assert_eq!(diffs[5].movie, "ppu-warmup,oam-decay");

        // Replaying with the same flags: nothing to report.
        let (same, diffs) = console_from_movie_header(&parsed.header, &config)
            .expect("reads back")
            .expect("names a console");
        assert_eq!(same, config);
        assert!(diffs.is_empty(), "{diffs:?}");

        // One field off: only that field is reported.
        let other_cli = ConsoleConfig::with_quirks(
            PowerOn {
                seed: 42,
                ..config.power_on
            },
            config.quirks,
        );
        let (_, diffs) = console_from_movie_header(&parsed.header, &other_cli)
            .expect("reads back")
            .expect("names a console");
        assert_eq!(diffs.len(), 1);
        assert_eq!(
            (
                diffs[0].key.as_str(),
                diffs[0].movie.as_str(),
                diffs[0].cli.as_str()
            ),
            ("nesrust_seed", "7", "42")
        );
    }

    #[test]
    fn the_overclock_is_recorded_and_reads_back() {
        let config = ConsoleConfig::new(PowerOn {
            overclock: 50,
            ..PowerOn::default()
        });
        let header = fm2_header_console(
            "zelda.nes",
            &rom_checksum(b"prg", b"chr"),
            &guid_from_seed(1),
            Region::Ntsc,
            &config,
            0,
        );
        assert!(header.contains(&("nesrust_overclock".to_owned(), "50".to_owned())));
        let text = Recorder::new(header).movie.to_fm2();
        assert!(text.contains("nesrust_overclock 50"), "{text}");
        let parsed = parse_fm2(&text).expect("a header-only movie parses");
        assert_eq!(ConsoleConfig::from_header(&parsed.header), Ok(Some(config)));
        // The overclock alone disagreeing is the one diff reported against a stock console.
        let (_, diffs) = console_from_movie_header(&parsed.header, &ConsoleConfig::default())
            .expect("reads back")
            .expect("names a console");
        let keys: Vec<&str> = diffs.iter().map(|d| d.key.as_str()).collect();
        assert_eq!(keys, vec!["nesrust_overclock"]);
        // A header without the key (an older recording) reads 0.
        let old = vec![("nesrust_seed".to_owned(), "3".to_owned())];
        let read = ConsoleConfig::from_header(&old).unwrap().unwrap();
        assert_eq!(read.power_on.overclock, 0);
        // An unreadable value is an error naming the key.
        let bad = vec![("nesrust_overclock".to_owned(), "x".to_owned())];
        assert!(
            ConsoleConfig::from_header(&bad)
                .unwrap_err()
                .contains("nesrust_overclock")
        );
    }

    #[test]
    fn a_movie_from_before_the_keys_keeps_the_command_line_console() {
        // Back-compat: a movie that says nothing about the console replays on the run's own.
        let text = "version 3\nemuName somenes\n|0|........|........||\n";
        let parsed = parse_fm2(text).expect("parses");
        assert_eq!(
            console_from_movie_header(&parsed.header, &ConsoleConfig::default()),
            Ok(None)
        );
        let cli = ConsoleConfig::with_quirks(
            PowerOn {
                ram: RamPowerOn::Ones,
                ..PowerOn::default()
            },
            Quirks {
                oam_decay: true,
                ..Quirks::default()
            },
        );
        assert_eq!(console_from_movie_header(&parsed.header, &cli), Ok(None));
    }

    #[test]
    fn a_movie_with_a_console_this_build_cannot_read_is_an_error() {
        let header = vec![
            ("version".to_owned(), "3".to_owned()),
            ("nesrust_quirks".to_owned(), "no-such-quirk".to_owned()),
        ];
        let err = console_from_movie_header(&header, &ConsoleConfig::default())
            .expect_err("an unknown quirk name must not be dropped");
        assert!(err.contains("no-such-quirk"), "{err}");
    }

    // -------------------------------------------------------------------------------------
    // the player presses Reset the way the recorder did.
    // -------------------------------------------------------------------------------------

    /// A minimal 16 KiB PRG + 8 KiB CHR NROM image, so a `Nes` can be built (and reset) here.
    fn blank_rom() -> Vec<u8> {
        let mut rom = vec![0x4E, 0x45, 0x53, 0x1A, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        rom.resize(16 + 16384 + 8192, 0);
        rom
    }

    #[test]
    fn a_default_player_resets_exactly_like_reset() {
        // A movie with one Reset frame and no console header: `Player::new` keeps the default
        // power-on options, whose reset alignment is phase 0 — the plain `reset()` it always did.
        let rom = blank_rom();
        let mut played = Nes::new(&rom).expect("a blank NROM loads");
        let mut plain = Nes::new(&rom).expect("a blank NROM loads");
        for _ in 0..2 {
            played.run_frame();
            plain.run_frame();
        }
        let movie = Movie {
            header: vec![],
            frames: vec![MovieFrame {
                commands: 0b01,
                pads: [0, 0],
            }],
        };
        assert_eq!(Player::new(movie).apply(&mut played), Ok(true));
        plain.reset();
        assert_eq!(played.save_state(), plain.save_state());
    }

    #[test]
    fn a_power_cycle_starts_the_reset_count_over() {
        // A Reset, then a frame that both power-cycles and resets: the press before the rebuild is
        // that machine's first, and the one on the rebuilt machine is its first again — not index 1.
        let rom = blank_rom();
        let mut nes = Nes::new(&rom).expect("a blank NROM loads");
        let power_on = PowerOn {
            reset_phase: PhaseChoice::Random,
            seed: 42,
            ..PowerOn::default()
        };
        let mut player = Player::with_power_on(
            Movie {
                header: vec![],
                frames: vec![
                    MovieFrame {
                        commands: 0b01,
                        pads: [0, 0],
                    },
                    MovieFrame {
                        commands: 0b11,
                        pads: [0, 0],
                    },
                ],
            },
            power_on,
        );
        assert_eq!(player.apply(&mut nes), Ok(true));
        assert_eq!(player.reset_index, 1);
        assert_eq!(
            player.apply(&mut nes),
            Err(MovieError::PowerCycleUnsupported)
        );
        player.apply_power_cycle();
        assert_eq!(player.reset_index, 0, "a rebuild is a fresh power-on");
        assert_eq!(
            player.apply(&mut nes),
            Ok(true),
            "the cleared frame's Reset still applies"
        );
        assert_eq!(player.reset_index, 1);
    }

    // -------------------------------------------------------------------------------------
    // BizHawk .bk2 import.
    //
    // Every .bk2 below is built here, in memory. No real .bk2 is committed to this repo (they
    // come from TASVideos submissions and are not redistributable), so these tests pin the
    // decoder against the format TASVideos documents, not against a sample file.
    // -------------------------------------------------------------------------------------

    /// Build a `.bk2` archive in memory from `(entry name, contents)` pairs.
    fn bk2_zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        for (name, bytes) in entries {
            writer.start_file(*name, options).expect("start an entry");
            writer.write_all(bytes).expect("write an entry");
        }
        writer.finish().expect("finish the archive").into_inner()
    }

    #[test]
    fn a_synthetic_bk2_decodes_four_frames_of_buttons() {
        // BizHawk's NES column order: the console's own buttons, then P1's, then P2's.
        let log = "[Input]\n\
                   LogKey:#Reset|Power|#P1 Up|P1 Down|P1 Left|P1 Right|P1 Start|P1 Select|P1 B|P1 A|\
                   #P2 Up|P2 Down|P2 Left|P2 Right|P2 Start|P2 Select|P2 B|P2 A|\n\
                   |..|........|........|\n\
                   |..|....T...|........|\n\
                   |R.|UDLRTSBA|........|\n\
                   |..|........|..L.....|\n";
        let movie = parse_bk2_bytes(&bk2_zip(&[
            (
                "Header.txt",
                b"MovieVersion BizHawk v2.0.0\nPlatform NES\nCore QuickNes\n",
            ),
            ("Input Log.txt", log.as_bytes()),
        ]))
        .expect("a synthetic .bk2 parses");

        assert_eq!(movie.frames.len(), 4);
        assert_eq!(
            movie.frames[0],
            MovieFrame {
                commands: 0,
                pads: [0, 0]
            }
        );
        // Position 4 of P1's group is Start.
        assert_eq!(
            movie.frames[1],
            MovieFrame {
                commands: 0,
                pads: [BUTTON_START, 0]
            }
        );
        // Every P1 button at once, plus the console's reset column.
        assert_eq!(movie.frames[2].commands, 1);
        assert_eq!(movie.frames[2].pads, [0xFF, 0]);
        // P2's Left is position 2 of the second pad group.
        assert_eq!(
            movie.frames[3],
            MovieFrame {
                commands: 0,
                pads: [0, BUTTON_LEFT]
            }
        );
        // The header comes across, so a diagnostic can name the core the movie came from.
        let core = movie.header.iter().find(|(key, _)| key == "Core");
        assert_eq!(core.map(|(_, value)| value.as_str()), Some("QuickNes"));
    }

    #[test]
    fn a_bk2_log_key_decides_the_column_order() {
        // The button columns are reversed and QuickNes's analog `Reset Cycle` knob leads, so
        // only a name-driven parse finds A at the front and Up at the back.
        let log = "[Input]\n\
                   LogKey:#Reset Cycle|Power|Reset|#P1 A|P1 B|P1 Select|P1 Start|P1 Right|\
                   P1 Left|P1 Down|P1 Up|\n\
                   |    0,..|A.......|\n\
                   |   12,X.|.......U|\n";
        let movie = parse_bk2_bytes(&bk2_zip(&[("Input Log.txt", log.as_bytes())]))
            .expect("a synthetic .bk2 parses");

        assert_eq!(movie.frames.len(), 2);
        assert_eq!(
            movie.frames[0],
            MovieFrame {
                commands: 0,
                pads: [BUTTON_A, 0]
            }
        );
        // The analog column is one column, not six buttons (its value is non-zero, so reading
        // it as buttons would press Reset too), and a press on Power is the frame's
        // power-cycle command.
        assert_eq!(
            movie.frames[1],
            MovieFrame {
                commands: 0b10,
                pads: [BUTTON_UP, 0]
            }
        );
    }

    #[test]
    fn bk2_entries_are_found_in_a_folder_and_in_any_case() {
        let log = "LogKey:#P1 B|\n|B|\n";
        let movie = parse_bk2_bytes(&bk2_zip(&[("nes/INPUT LOG.TXT", log.as_bytes())]))
            .expect("an entry is matched by its own name, not its path or case");
        assert_eq!(
            movie.frames,
            vec![MovieFrame {
                commands: 0,
                pads: [BUTTON_B, 0]
            }]
        );
    }

    #[test]
    fn a_bk2_without_an_input_log_is_a_named_error() {
        let err = parse_bk2_bytes(&bk2_zip(&[("Header.txt", b"Platform NES\n")]))
            .expect_err("an archive with no input log has nothing to play");
        assert_eq!(err, MovieError::Bk2NoInputLog);
        assert!(err.to_string().contains("Input Log.txt"), "{err}");
    }

    #[test]
    fn a_bk2_that_starts_from_a_savestate_is_refused_by_name() {
        // The log is valid, but its first frame is a state's frame rather than the game's:
        // playing it from power-on would desync, so it is refused instead of played wrongly.
        let log = "LogKey:#P1 A|\n|A|\n";
        for entry in ["Core.bin", "CoreState", "CoreState.bin"] {
            let err = parse_bk2_bytes(&bk2_zip(&[
                ("Input Log.txt", log.as_bytes()),
                (entry, b"savestate"),
            ]))
            .expect_err("a savestate-anchored .bk2 cannot be played from power-on");
            assert_eq!(err, MovieError::Bk2Savestate, "{entry}");
            assert!(err.to_string().contains("savestate"), "{entry}: {err}");
        }
    }

    #[test]
    fn a_garbage_bk2_input_log_names_the_offending_line() {
        // Three columns where the key names eight.
        let log = "[Input]\n\
                   LogKey:#P1 Up|P1 Down|P1 Left|P1 Right|P1 Start|P1 Select|P1 B|P1 A|\n|...|\n";
        let err = parse_bk2_bytes(&bk2_zip(&[("Input Log.txt", log.as_bytes())]))
            .expect_err("a frame line that disagrees with the LogKey is malformed");
        assert_eq!(err, MovieError::BadLine(3));
    }

    #[test]
    fn a_bk2_input_log_without_a_log_key_or_frames_is_a_named_error() {
        let err = parse_bk2_bytes(&bk2_zip(&[("Input Log.txt", b"[Input]\n|........|\n")]))
            .expect_err("columns with no names cannot be decoded by position");
        assert_eq!(err, MovieError::Bk2NoLogKey);

        let err = parse_bk2_bytes(&bk2_zip(&[("Input Log.txt", b"[Input]\nLogKey:#P1 A|\n")]))
            .expect_err("a log with no frame lines plays nothing");
        assert_eq!(err, MovieError::Bk2NoFrames);
    }

    #[test]
    fn a_file_that_is_not_a_zip_is_a_named_bk2_error() {
        let err = parse_bk2_bytes(b"not a zip at all").expect_err("not a zip");
        assert!(matches!(err, MovieError::Bk2Archive(_)), "{err:?}");
        assert!(err.to_string().contains(".bk2 archive"), "{err}");
    }
}
