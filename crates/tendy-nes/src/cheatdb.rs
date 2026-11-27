//! NES cheat database reader (built-in code lists).
//!
//! The database format is RetroArch's `.cht`: one `key = value` line per setting, cheats
//! numbered from zero, and each cheat's codes written either as Game Genie letters or as a raw
//! `AAAA:VV` address and byte. That is RetroArch's `config_file` format, loaded by
//! `cheat_manager_load` in
//! <https://github.com/libretro/RetroArch/blob/master/cheat_manager.c>, and it is the format of
//! the NES files libretro-database publishes under
//! `cht/Nintendo - Nintendo Entertainment System`
//! (<https://github.com/libretro/libretro-database>).
//!
//! FCEUX does not read or write this format: `FCEU_LoadGameCheats` (`fceux/src/cheat.cpp`)
//! parses `[S][C][:]AAAA:VV[:CC]:name` lines, a notation with no `key = value` at all. Reading
//! an FCEUX cheat file is its own format, and nothing here guesses at one.
//!
//! ```text
//! cheats = 3
//! cheat0_desc = "Both Players Start With 9 Rolls"
//! cheat0_code = "PASIOALE"
//! cheat0_enable = false
//! cheat1_desc = "Both Players Start With 9 Rolls, Alternative"
//! cheat1_code = "OZOYUEPX+OZVULSPX"
//! cheat1_enable = false
//! cheat2_desc = "Infinite time"
//! cheat2_code = "0025:63"
//! cheat2_enable = false
//! ```
//!
//! That vocabulary came from the corpus, not from prose. The whole libretro-database directory
//! — 2262 `.cht` files — reads through [`load_dir`] as 2179 records holding 42,981 entries and
//! 53,722 codes; 83 files are refused with the file and line that stopped them (76
//! [`ParseError::BadCode`], 7 [`ParseError::EmptyCode`]), and 106 entries import without their
//! codes because the file asked for something a byte substitution cannot do (102 of them a
//! disabled `cheat_type`, 4 a `repeat_count` above one). Those numbers are the whole directory,
//! not a sample.
//!
//! # What a cheat means to RetroArch
//!
//! RetroArch reads `cheatN_address` and `cheatN_value` for every cheat, and applies them only
//! through its own handler: `cheat_manager_apply_retro_cheats` skips every cheat whose
//! `handler` is not 1, then acts on `cheat_type` — 0 disabled, 1 set to the value, 2 and 3 add
//! and subtract, 4-7 are run-next-if conditions — reading an item `memory_search_size` deep
//! (3 is one byte) and writing `repeat_count` of them from the address on. The 57 `(Rumbles)`
//! files in the corpus set `cheat_type = 0`: their 102 entries are rumble triggers that write
//! nothing at all, and importing one as a RAM substitution would make a controller rumble
//! change the game.
//!
//! A [`Code`] here is one byte substituted at one address, so an address/value cheat becomes a
//! code only for the combination RetroArch applies as exactly that: `handler = 1`,
//! `cheat_type = 1`, `memory_search_size = 3`, a `repeat_count` of one, and no partial
//! `address_bit_position`. Anything else — a disabled placeholder, a 16-bit write, an add, a
//! stored condition, a repeated write, a bit mask — is imported as an entry with its
//! description kept and no codes, and [`Entry::note`] says which line and which key stopped it.
//! A file that names none of those keys gets no invented write either: only a file saying
//! `handler = 1` reaches that path in RetroArch.
//!
//! Codes are decoded into [`crate::genie::Code`], the form the Game Genie decoder already
//! produces (`addr`, `value`, `compare`), so an imported cheat can be applied with
//! [`crate::genie::Code::apply`] and no further translation:
//!
//! * A Game Genie string goes through [`crate::genie::decode`], which owns the bit shuffle.
//!   Six letters give a code that always substitutes; eight letters also carry a compare
//!   byte and only substitute when the byte on the bus matches it.
//! * A raw `AAAA:VV` code is its address and byte as written — `0025:63` is value `$63` at
//!   RAM address `$0025`. The database means "the game must see `$63` at `$0025`", which is
//!   what read substitution at that address gives; expressing it as a [`Code`] keeps one
//!   apply path for both notations instead of a second one for RAM pokes. Unlike a
//!   write-every-frame RAM cheat, a substituted read cannot be overwritten by the game between
//!   frames.
//! * A cheat with several codes applies all of them; each is a separate [`Code`].
//!
//! # Record keys
//!
//! A `.cht` file carries no ROM hash: RetroArch and snes9x build the cheat file's path from the
//! ROM's name, and a name is what a shipped file keys its record by. Names drift (a renamed or
//! region-labelled ROM loses its cheats, two ROMs with one name share a set), so this module
//! keys records by the **MD5 of the ROM's data** whenever the file says so, the digest
//! [`crate::checksum`] already computes and [`crate::movie`] puts in an FM2 `romChecksum`
//! header. Two extra lines carry that, and both are optional:
//!
//! ```text
//! name = Super Mario Bros.
//! hash = 900150983cd24fb0d6963f7d28e17f72
//! ```
//!
//! A record with a `hash` line is found by checksum with [`lookup`]; a record with only a
//! name (every shipped `.cht` file) is found by name with [`lookup_name`], and [`load_file`]
//! names a nameless record after the file it came from. One file may hold several records by
//! repeating `hash` or `name`, which is how one database can cover a whole collection. A record
//! ends at a second `hash`, a second `name`, or either one after cheats — never because of the
//! order two keys happen to arrive in, so a `hash` before the `name` is one record keyed both
//! ways rather than a hashed record with no cheats and a named one with no hash.
//!
//! # The grammar this module reads
//!
//! * Blank lines and lines starting with `#` are skipped, and a leading UTF-8 BOM is
//!   dropped (the files carry one from Windows).
//! * Every other line is `keyword = value`. Keywords are case-insensitive, space around `=`
//!   is free, and a value may be quoted with `"` — the corpus quotes every value.
//! * `cheats = N` declares a record's cheat count and is checked against what was read. Some
//!   shipped files quote the number (`cheats = "2"`); both spellings read the same.
//! * `cheatN_desc`, `cheatN_code`, `cheatN_enable`, `cheatN_address`, `cheatN_value`,
//!   `cheatN_handler`, `cheatN_cheat_type`, `cheatN_memory_search_size`,
//!   `cheatN_address_bit_position` and `cheatN_repeat_count` address cheat `N`. Cheats may be
//!   numbered out of order or with gaps; they are kept in the order the file lists them.
//! * `desc`, `code`, `enable`, `address` and `value` without a number address the cheat being
//!   written. A second bare `desc` starts the next cheat. The RetroArch keys above are numbered
//!   only: a bare `handler` is an unknown keyword.
//! * `cheatN_code` holds one code or several joined by `+`; an empty `cheatN_code` is allowed
//!   and says the cheat is written with `address` and `value` instead, or that the entry is a
//!   note. Shipped databases put headings and comments in the list that way — a `desc` with
//!   no code — and `cheats = N` counts them, so they are kept as an [`Entry`] with no codes
//!   rather than rejected. An entry with neither a description nor a code is an error. A cheat
//!   that gives both a code string and the RetroArch keys is read for its code; no shipped file
//!   does that.
//! * `cheatN_address` and `cheatN_value` are numbers in the base `strtoul` picks with base 0:
//!   `0x` for hex, a leading `0` for octal, decimal otherwise — the base RetroArch passes.
//! * `cheatN_enable` is `true`/`false` (or `1`/`0`). A cheat with no `enable` line loads
//!   switched off, because importing a database must not change the game by itself.
//! * `author`, `sortname`, `display_name`, `version`, `requires`, `cheat_events`, `autorun`
//!   and `control`/`control1`/`control2` are read and dropped: real files carry them and they
//!   say nothing about the codes. So is any other `cheatN_` keyword this module does not
//!   model (`cheatN_big_endian`, the `cheatN_repeat_add_*` and `cheatN_rumble_*` families).
//! * A keyword that is neither recognised nor a `cheatN_` field is an error, so a typo is
//!   reported instead of silently ignored.
//!
//! # What this module does not read
//!
//! Mesen's `.xml` cheat files, the other notation in the same directory
//! (`<cheats version="1.0">` with `<genie>`, `<address>`, `<value>` and `<compare>` children),
//! are refused with [`ParseError::NotCht`] rather than half-read; [`load_dir`] skips them by
//! extension. FCEUX's `[S][C]` lines are a third notation and are not read either. Two rarer
//! notations in the corpus are refused with the file and line that carry them instead of
//! guessed at, because a wrong address is worse than a reported one: Game Genie codes written
//! as hex nibbles with `?` wildcards (`01??65C0`, `0007C507`) and bare numbers (`509`). A cheat
//! is imported as single bytes, so a multi-byte `cheatN_value` is an error
//! ([`ParseError::BadValue`]) rather than a truncation. Nothing here touches the bus: reading
//! a database and applying a code are separate steps, and the wiring arrives later.
use std::collections::HashMap;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

use crate::cart::RomData;
use crate::checksum::md5;
use crate::genie::{Code, GenieError, decode};

/// One cheat: what it is called, the codes it applies, and whether it is switched on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// The description the database gives, `""` when the file has no `desc` line.
    pub desc: String,
    /// Every code this cheat applies, in the order the database lists them. A Game Genie
    /// cheat has one, a `+`-joined cheat has several, and an `address`/`value` cheat has the
    /// one code those two lines spell. Empty for a note — a described entry with no code,
    /// which shipped databases use as a heading — and for an entry whose codes were refused,
    /// which [`Entry::note`] explains.
    pub codes: Vec<Code>,
    /// Whether the database switched this cheat on.
    pub enabled: bool,
    /// Why this entry carries no codes, when the file asked for codes this module will not
    /// apply. `None` for an ordinary entry. A note keeps the description and names the line
    /// that decided it, so a refused cheat is reported rather than guessed at.
    pub note: Option<String>,
}

/// One game's record in the database.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    /// MD5 of the ROM's data when the file carries a `hash =` line; `None` for a record keyed
    /// by name only, which is every `.cht` file as shipped.
    pub hash: Option<[u8; 16]>,
    /// Display title from `name =`, or the file's stem when [`load_file`] filled it in.
    pub name: String,
    /// The record's cheats, in the order the file lists them.
    pub entries: Vec<Entry>,
}

impl Record {
    /// Whether this record describes the ROM whose data hashes to `digest`.
    pub fn matches(&self, digest: [u8; 16]) -> bool {
        self.hash == Some(digest)
    }
}

/// Why a text file is not a cheat database this module reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    /// An XML cheat file: the other notation, deliberately not read here.
    NotCht,
    /// A line that is not `keyword = value`, or names a keyword this format has no meaning
    /// for. Carries the line number and the line as written.
    UnknownKeyword(usize, String),
    /// `keyword =` with nothing after the `=`.
    NoValue(usize, String),
    /// `hash =` not written as 32 hex characters.
    BadHash(usize, String),
    /// `cheats =` not written as a number, or one of the RetroArch keys this module reads
    /// (`handler`, `cheat_type`, `memory_search_size`, `address_bit_position`,
    /// `repeat_count`).
    BadNumber(usize, String),
    /// `enable =` not written as `true`, `false`, `1` or `0`.
    BadEnable(usize, String),
    /// A code the Game Genie decoder rejects, with the line as written and the decoder's own
    /// reason.
    BadCode(usize, String, GenieError),
    /// An `address`/`value` cheat whose value does not fit in a byte, or whose address does
    /// not fit in 16 bits.
    BadValue(usize, String),
    /// A cheat with no code, carrying the line that opened it and its description.
    EmptyCode(usize, String),
    /// A record with neither a `hash` nor a `name`, so nothing keys it.
    MissingKey(String),
    /// The record's `cheats = N` disagreed with the cheats found.
    CountMismatch(String, usize, usize),
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ParseError::NotCht => write!(
                f,
                "this is an XML cheat file; only the key = value .cht format is read"
            ),
            ParseError::UnknownKeyword(number, line) => {
                write!(f, "line {number} is not a cheat database line: {line:?}")
            }
            ParseError::NoValue(number, line) => {
                write!(f, "line {number} has no value: {line:?}")
            }
            ParseError::BadHash(number, value) => write!(
                f,
                "line {number}: a ROM hash is 32 hex characters, {value:?} is not"
            ),
            ParseError::BadNumber(number, value) => {
                write!(f, "line {number}: {value:?} is not a number")
            }
            ParseError::BadEnable(number, value) => {
                write!(f, "line {number}: {value:?} is not true or false")
            }
            ParseError::BadCode(number, line, why) => {
                write!(f, "line {number}: {line:?} is not a cheat code: {why}")
            }
            ParseError::BadValue(number, value) => write!(
                f,
                "line {number}: {value:?} does not fit in an address and a byte"
            ),
            ParseError::EmptyCode(number, desc) => {
                write!(f, "line {number}: the cheat {desc:?} has no code")
            }
            ParseError::MissingKey(name) => {
                write!(f, "the record {name:?} has no ROM hash and no name")
            }
            ParseError::CountMismatch(name, declared, found) => {
                write!(
                    f,
                    "the record {name:?} declares {declared} cheats but has {found}"
                )
            }
        }
    }
}

impl std::error::Error for ParseError {}

/// A database that could not be read, or was read and is not one. Every variant names the
/// file it came from, so a refusal in a directory of thousands says which one to look at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoadError {
    /// The file or directory could not be opened, as the operating system put it.
    Read(PathBuf, String),
    /// The bytes are not a cheat database this module reads, and the parse error that says
    /// why.
    Malformed(PathBuf, ParseError),
}

impl LoadError {
    /// The file or directory this error is about.
    pub fn path(&self) -> &Path {
        match self {
            LoadError::Read(path, _) | LoadError::Malformed(path, _) => path,
        }
    }
}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: ", self.path().display())?;
        match self {
            LoadError::Read(_, why) => write!(f, "{why}"),
            LoadError::Malformed(_, why) => write!(f, "{why}"),
        }
    }
}

impl std::error::Error for LoadError {}

/// What [`load_dir`] found: the records it could read, and one error per file it could not.
/// A directory of thousands of files always holds a few this module refuses, and one of them
/// must not cost the caller every good record beside it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Loaded {
    /// Every record read, in filename order.
    pub records: Vec<Record>,
    /// How many `.cht` files were attempted.
    pub files: usize,
    /// One error per file that refused, each naming its path.
    pub failures: Vec<LoadError>,
}

impl Loaded {
    /// How many files were read without a refusal.
    pub fn loaded(&self) -> usize {
        self.files.saturating_sub(self.failures.len())
    }

    /// How many files refused.
    pub fn refused(&self) -> usize {
        self.failures.len()
    }
}

/// A database embedded in the crate: two records in the shipped `.cht` vocabulary, one keyed
/// by `hash` and one by `name`. Copy it next to a ROM as `cheats/<rom>.cht` and edit the
/// codes. The first record's cheats use the numbered spelling libretro-database ships, the
/// second the bare spelling; every code in it is a real one from that database.
pub const SAMPLE_DB: &str = r#"# A cheat database as nes-rust reads it. One record per game.
name = Super Mario Bros.
hash = 900150983cd24fb0d6963f7d28e17f72

cheats = 3
cheat0_desc = "Infinite lives"
cheat0_code = "SXIOPO"
cheat0_enable = false

cheat1_desc = "Invincibility"
cheat1_code = "SLXPLOVS"
cheat1_enable = false

cheat2_desc = "Two codes for one cheat"
cheat2_code = "OZOYUEPX+OZVULSPX"
cheat2_enable = false

# The second record is keyed by name alone, the way a shipped .cht file is, and writes its
# cheats with bare keywords instead of numbers.
name = 1942
author = libretro-database

desc = "Both Players Start With 9 Rolls"
code = "PASIOALE"
enable = false

desc = "Infinite time"
code = "0025:63"
enable = false
"#;

/// Keywords real `.cht` files carry that say nothing about the codes.
const RECORD_IGNORED: &[&str] = &[
    "author",
    "sortname",
    "display_name",
    "version",
    "requires",
    "cheat_events",
    "autorun",
    "control",
    "control1",
    "control2",
];

/// The keyword a cheat line writes, for picking the cheat a bare keyword belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Field {
    Desc,
    Code,
    Enable,
    Address,
    Value,
}

/// A cheat while the file is still being read: the code has not been built from the
/// `address`/`value` pair yet. Which number named it is kept in the [`Draft`]'s map.
#[derive(Debug, Default)]
struct Cheat {
    /// The line that first named this cheat, so a refusal points at the cheat and not only at
    /// the file.
    line: usize,
    desc: Option<String>,
    codes: Vec<Code>,
    address: Option<u16>,
    value: Option<u8>,
    enabled: bool,
    retro: Retro,
    /// Why the cheat has no code, when its `code` line would not decode. The entry is kept so
    /// the file still reads and the cheat is reported as not applied.
    note: Option<String>,
}

/// The RetroArch keys that decide whether an `address`/`value` cheat is applied and how. Each
/// keeps the line it was written on, so an entry that is not imported can say which line said
/// so. `None` means the file did not say, which is not the same as RetroArch's default.
#[derive(Debug, Default)]
struct Retro {
    handler: Option<(u64, usize)>,
    cheat_type: Option<(u64, usize)>,
    memory_search_size: Option<(u64, usize)>,
    address_bit_position: Option<(u64, usize)>,
    repeat_count: Option<(u64, usize)>,
}

impl Retro {
    /// Why RetroArch would not apply this cheat's `address`/`value` as one byte substituted at
    /// one address, or `None` when it would. The rules are
    /// `cheat_manager_load_cb_second_pass` and `cheat_manager_apply_retro_cheats` in
    /// RetroArch's `cheat_manager.c`: only `handler` 1 reaches the address/value path, its
    /// effect comes from `cheat_type` (0 is disabled, 1 sets the value, 2 and 3 add and
    /// subtract, 4-7 are run-next-if conditions), the width comes from `memory_search_size`
    /// (3 is one byte), and `repeat_count` writes that many items from the address on.
    fn unapplied(&self, line: usize) -> Option<String> {
        let Some((kind, kind_line)) = self.cheat_type else {
            return Some(format!(
                "line {line}: the file gives no cheat_type, and only cheat_type 1 (set to value) is a fixed byte"
            ));
        };
        match kind {
            0 => {
                return Some(format!(
                    "line {kind_line}: cheat_type 0 is a disabled placeholder, so RetroArch writes nothing; \
                     the description is kept and no code is imported"
                ));
            }
            1 => {}
            other => {
                return Some(format!(
                    "line {kind_line}: cheat_type {other} is {}, which a fixed byte substitution cannot express",
                    cheat_type_name(other)
                ));
            }
        }
        let Some((handler, handler_line)) = self.handler else {
            return Some(format!(
                "line {line}: the file gives no handler, and RetroArch applies an address and value only through handler 1"
            ));
        };
        if handler != 1 {
            return Some(format!(
                "line {handler_line}: handler {handler} is the emulator's own cheat handler; the address/value path is handler 1"
            ));
        }
        let Some((width, width_line)) = self.memory_search_size else {
            return Some(format!(
                "line {line}: the file gives no memory_search_size, and only 3 (one 8-bit byte) is a single byte"
            ));
        };
        if width != 3 {
            return Some(format!(
                "line {width_line}: memory_search_size {width} is not 3, so the cheat is not one 8-bit byte"
            ));
        }
        if let Some((repeat, repeat_line)) = self.repeat_count.filter(|&(repeat, _)| repeat != 1) {
            return Some(format!(
                "line {repeat_line}: repeat_count {repeat} writes more than one byte from its address; \
                 a single code writes one"
            ));
        }
        // `address_bit_position` selects bits inside the item, and RetroArch reads it only when
        // the width is under 8 bits. Every cheat that reaches here is one whole byte, so a
        // field of 0 or 255 (the whole byte) says the same thing the code does; a partial mask
        // selects bits a single substitution cannot write.
        if let Some((position, position_line)) = self
            .address_bit_position
            .filter(|&(position, _)| position != 0 && position != 0xFF)
        {
            return Some(format!(
                "line {position_line}: address_bit_position {position} selects bits inside the byte, \
                 which a whole-byte substitution cannot express"
            ));
        }
        None
    }
}

/// RetroArch's own name for a `cheat_type` this module does not import.
fn cheat_type_name(kind: u64) -> &'static str {
    match kind {
        2 => "an add to the current value",
        3 => "a subtract from the current value",
        4..=7 => "a run-next-if condition",
        _ => "not a cheat type RetroArch has",
    }
}

/// A record while the file is still being read.
#[derive(Debug, Default)]
struct Draft {
    hash: Option<[u8; 16]>,
    name: String,
    declared: Option<usize>,
    cheats: Vec<Cheat>,
    /// Where the cheat each number names sits in `cheats`. A scan per line made reading a
    /// 200,000-cheat file take over a minute.
    numbers: HashMap<usize, usize>,
    /// Where the last cheat opened by a bare keyword sits in `cheats`.
    bare: Option<usize>,
}

/// Read a whole database into records.
pub fn parse(text: &str) -> Result<Vec<Record>, ParseError> {
    parse_with(text, None)
}

/// Read a whole database, naming any record that carries no `name =` line after `fallback` —
/// which is how [`load_file`] gives a shipped `.cht` the name of its game.
fn parse_with(text: &str, fallback: Option<&str>) -> Result<Vec<Record>, ParseError> {
    // A database that starts with a tag is Mesen's XML notation, not this one. The check comes
    // first so the report names the format instead of quoting `<?xml` as a bad keyword.
    if is_xml(text) {
        return Err(ParseError::NotCht);
    }
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);

    let mut records = Vec::new();
    let mut draft = Draft::default();
    for (offset, raw) in text.lines().enumerate() {
        let number = offset + 1;
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            return Err(ParseError::UnknownKeyword(number, line.to_owned()));
        };
        let keyword = key.trim().to_ascii_lowercase();
        let value = value.trim();
        if RECORD_IGNORED.contains(&keyword.as_str()) {
            continue;
        }
        if value.is_empty() {
            return Err(ParseError::NoValue(number, line.to_owned()));
        }

        let (field, index) = split_keyword(&keyword);
        match field {
            // `hash` and `name` head a record, and a record ends when the file starts another
            // one: a second `hash`, a second `name`, or either one after cheats. The key order
            // must not decide that, because `hash` is our addition and may be written before
            // the `name` it belongs with.
            "hash" => {
                if draft.hash.is_some() || !draft.cheats.is_empty() {
                    records.push(finish(std::mem::take(&mut draft), fallback)?);
                }
                let hash = parse_hash(value)
                    .ok_or_else(|| ParseError::BadHash(number, value.to_owned()))?;
                draft.hash = Some(hash);
            }
            "name" => {
                // Only a draft that already has a name or cheats is closed here: `hash` then
                // `name` is one record keyed both ways, not a hashed record and an empty one.
                if !draft.name.is_empty() || !draft.cheats.is_empty() {
                    records.push(finish(std::mem::take(&mut draft), fallback)?);
                }
                draft.name = unquote(value);
            }
            "cheats" => {
                let declared = parse_number(&unquote(value))
                    .and_then(|count| usize::try_from(count).ok())
                    .ok_or_else(|| ParseError::BadNumber(number, unquote(value)))?;
                draft.declared = Some(declared);
            }
            "desc" => {
                let at = entry_for(&mut draft, Field::Desc, index, number);
                draft.cheats[at].desc = Some(unquote(value));
            }
            "code" => {
                let text = unquote(value);
                if text.is_empty() {
                    // An empty `code` says the cheat is written with `address`/`value`.
                    entry_for(&mut draft, Field::Code, index, number);
                    continue;
                }
                let at = entry_for(&mut draft, Field::Code, index, number);
                // A code that will not decode is one cheat's problem, not the file's: the cheat is
                // kept with the decoder's reason, and the rest of the file reads.
                match decode_codes(number, line, &text) {
                    Ok(codes) => draft.cheats[at].codes = codes,
                    Err(why) => draft.cheats[at].note = Some(why.to_string()),
                }
            }
            "enable" => {
                let on = parse_enable(value)
                    .ok_or_else(|| ParseError::BadEnable(number, value.to_owned()))?;
                let at = entry_for(&mut draft, Field::Enable, index, number);
                draft.cheats[at].enabled = on;
            }
            "address" => {
                let text = unquote(value);
                let address = parse_number(&text)
                    .and_then(|address| u16::try_from(address).ok())
                    .ok_or_else(|| ParseError::BadValue(number, text.clone()))?;
                let at = entry_for(&mut draft, Field::Address, index, number);
                draft.cheats[at].address = Some(address);
            }
            "value" => {
                let text = unquote(value);
                let byte = parse_number(&text)
                    .and_then(|byte| u8::try_from(byte).ok())
                    .ok_or_else(|| ParseError::BadValue(number, text.clone()))?;
                let at = entry_for(&mut draft, Field::Value, index, number);
                draft.cheats[at].value = Some(byte);
            }
            // The RetroArch keys that decide what an `address`/`value` cheat does. They are
            // numbered metadata — `cheat0_cheat_type`, never bare — and are read rather than
            // dropped, because `cheat0_cheat_type = 0` alone turns a live RAM write into a
            // placeholder that writes nothing.
            "handler"
            | "cheat_type"
            | "memory_search_size"
            | "address_bit_position"
            | "repeat_count" => {
                let Some(named) = index else {
                    return Err(ParseError::UnknownKeyword(number, line.to_owned()));
                };
                let parsed = parse_number(&unquote(value))
                    .ok_or_else(|| ParseError::BadNumber(number, unquote(value)))?;
                let at = numbered(&mut draft, named, number);
                let retro = &mut draft.cheats[at].retro;
                let slot = match field {
                    "handler" => &mut retro.handler,
                    "cheat_type" => &mut retro.cheat_type,
                    "memory_search_size" => &mut retro.memory_search_size,
                    "address_bit_position" => &mut retro.address_bit_position,
                    _ => &mut retro.repeat_count,
                };
                *slot = Some((parsed, number));
            }
            // A numbered keyword this module does not model is metadata for a cheat
            // (`cheat0_rumble_port`), not a mistake; a bare one is a mistake.
            _ if index.is_some() => continue,
            _ => return Err(ParseError::UnknownKeyword(number, line.to_owned())),
        }
    }

    if draft.hash.is_some() || !draft.name.is_empty() || !draft.cheats.is_empty() {
        records.push(finish(draft, fallback)?);
    }
    Ok(records)
}

/// Read a database from bytes. A `.cht` file is bytes, not necessarily UTF-8 — an author name
/// in Western European text lands on an 8-bit code page — so bytes that are not valid UTF-8
/// are read one character per byte, which keeps the ASCII codes intact.
pub fn parse_bytes(data: &[u8]) -> Result<Vec<Record>, ParseError> {
    parse_bytes_with(data, None)
}

/// [`parse_bytes`] with the name a record without a `name =` line should take.
fn parse_bytes_with(data: &[u8], fallback: Option<&str>) -> Result<Vec<Record>, ParseError> {
    match std::str::from_utf8(data) {
        Ok(text) => parse_with(text, fallback),
        Err(_) => {
            let text: String = data.iter().map(|&byte| char::from(byte)).collect();
            parse_with(&text, fallback)
        }
    }
}

/// Read one database file. A record with no `name =` line is named after the file, which is
/// how a shipped `.cht` names its game. A refusal carries the file's path and the line it was
/// read on, so it says where to look.
pub fn load_file(path: &Path) -> Result<Vec<Record>, LoadError> {
    let data =
        std::fs::read(path).map_err(|why| LoadError::Read(path.to_path_buf(), why.to_string()))?;
    let stem = path.file_stem().and_then(|stem| stem.to_str());
    parse_bytes_with(&data, stem).map_err(|why| LoadError::Malformed(path.to_path_buf(), why))
}

/// Read every `.cht` file in `dir`, in filename order, and keep going when one refuses: the
/// returned [`Loaded`] carries the records that were read, how many files were attempted, and
/// one error naming each file that was not. A directory that is not there is not an error — it
/// means the user has no cheat database yet, the same rule [`crate::save::load_save`] follows
/// for a missing `.sav`. Other extensions are skipped, so the XML files Mesen keeps in the same
/// folder do not stop the load. `Err` is only for a directory that exists and cannot be read.
pub fn load_dir(dir: &Path) -> Result<Loaded, LoadError> {
    let mut loaded = Loaded::default();
    for path in cht_files(dir)?.unwrap_or_default() {
        loaded.files += 1;
        match load_file(&path) {
            Ok(mut records) => loaded.records.append(&mut records),
            Err(why) => loaded.failures.push(why),
        }
    }
    Ok(loaded)
}

/// The `.cht` files in `dir`, in filename order; `None` when there is no such directory, which
/// means the user has no cheat database yet. `Err` is a directory that exists and cannot be
/// listed - or a `cheats` that is a file.
pub fn cht_files(dir: &Path) -> Result<Option<Vec<PathBuf>>, LoadError> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(why) if why.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(why) => return Err(LoadError::Read(dir.to_path_buf(), why.to_string())),
    };
    let mut paths: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| ext.eq_ignore_ascii_case("cht"))
        })
        .collect();
    paths.sort();
    Ok(Some(paths))
}

/// [`load_file`] for one ROM: `Ok(None)`, without parsing, when the file cannot hold a record
/// [`lookup`] or [`lookup_name`] would pick for the ROM whose digest is `digest` (`None` for a
/// disk image, which has none) and whose file stem is `name`. A file can hold one when it is
/// named after the ROM (its nameless records take that name), or when its text spells the
/// digest in hex or the name, ignoring ASCII case as the lookups do. Everything else is another
/// game's file: reading the bytes is all it costs, so a launch parses only the files that matter.
pub fn load_file_for(
    path: &Path,
    digest: Option<[u8; 16]>,
    name: &str,
) -> Result<Option<Vec<Record>>, LoadError> {
    let data =
        std::fs::read(path).map_err(|why| LoadError::Read(path.to_path_buf(), why.to_string()))?;
    let stem = path.file_stem().and_then(|stem| stem.to_str());
    if !stem.is_some_and(|stem| stem.eq_ignore_ascii_case(name)) && !mentions(&data, digest, name) {
        return Ok(None);
    }
    parse_bytes_with(&data, stem)
        .map(Some)
        .map_err(|why| LoadError::Malformed(path.to_path_buf(), why))
}

/// Whether `data` spells `digest` as hex or contains `name`, ignoring ASCII case. A name that is
/// not ASCII always counts as found: a file that is not UTF-8 is read one character per byte, so
/// its bytes need not spell the name the way the name's own UTF-8 does.
fn mentions(data: &[u8], digest: Option<[u8; 16]>, name: &str) -> bool {
    if !name.is_ascii() {
        return true;
    }
    let text = data.to_ascii_lowercase();
    let contains = |needle: &[u8]| text.windows(needle.len()).any(|window| window == needle);
    digest.is_some_and(|digest| contains(digest_hex(digest).as_bytes()))
        || (!name.is_empty() && contains(name.to_ascii_lowercase().as_bytes()))
}

/// A digest as a `hash =` line spells it: 32 lower-case hex characters.
pub fn digest_hex(digest: [u8; 16]) -> String {
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Where a ROM's cheats live: `games/zelda.nes` -> `games/cheats`. The same
/// neighbour-of-the-ROM rule [`crate::save::save_path`] and [`crate::save::state_path`] use.
pub fn cheats_dir(rom: &Path) -> PathBuf {
    rom.parent().unwrap_or_else(|| Path::new("")).join("cheats")
}

/// The database file a ROM's cheats would be written in: `games/zelda.nes` ->
/// `games/cheats/zelda.cht`, the name RetroArch builds from the ROM's file name.
pub fn db_path(rom: &Path) -> PathBuf {
    let stem = rom.file_stem().unwrap_or_default();
    cheats_dir(rom).join(stem).with_extension("cht")
}

/// The record whose `hash` line matches `digest`. The first record wins if a database gives
/// two records the same hash; [`lookup_name`] is the fallback for a database keyed by filename.
pub fn lookup(records: &[Record], digest: [u8; 16]) -> Option<&Record> {
    records.iter().find(|record| record.matches(digest))
}

/// The record with no `hash` line whose name matches `name`, ignoring case. This is the
/// fallback for a database keyed by filename, where renaming the ROM loses its cheats — prefer
/// [`lookup`]. A record that carries a hash names the one ROM it belongs to, so it is never
/// found by name: a `smb.cht` holding another game's hash must not cheat Super Mario Bros.
pub fn lookup_name<'a>(records: &'a [Record], name: &str) -> Option<&'a Record> {
    records
        .iter()
        .find(|record| record.hash.is_none() && record.name.eq_ignore_ascii_case(name))
}

/// The MD5 of ROM data. Pass the bytes the cartridge holds, PRG then CHR — the same bytes
/// [`crate::movie::rom_checksum_for`] hashes, so a cheat record and an FM2 header agree on
/// which ROM they describe.
pub fn rom_digest(data: &[u8]) -> [u8; 16] {
    md5(data)
}

/// The digest of a loaded cartridge: PRG then CHR ROM, and no CHR bytes at all for a CHR-RAM
/// board, whose "CHR" is a zero image rather than ROM data. This is the key [`lookup`] wants.
pub fn cartridge_digest(data: &RomData) -> [u8; 16] {
    let chr: &[u8] = if data.chr_is_ram { &[] } else { &data.chr };
    let mut bytes = Vec::with_capacity(data.prg.len() + chr.len());
    bytes.extend_from_slice(&data.prg);
    bytes.extend_from_slice(chr);
    rom_digest(&bytes)
}

/// The cheat `keyword` addresses as (field, number); a bare keyword as (field, `None`).
fn split_keyword(keyword: &str) -> (&str, Option<usize>) {
    let Some(rest) = keyword.strip_prefix("cheat") else {
        return (keyword, None);
    };
    let Some((digits, field)) = rest.split_once('_') else {
        return (keyword, None);
    };
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return (keyword, None);
    }
    match digits.parse::<usize>() {
        Ok(number) => (field, Some(number)),
        // A number too long for `usize` names no cheat, so the line is unknown rather than an
        // error about the number.
        Err(_) => (keyword, None),
    }
}

/// The cheat a keyword belongs to: the one already carrying that number, or — for a bare
/// keyword — the cheat being written. An unseen number opens a new cheat, so files may number
/// their cheats out of order or with gaps.
fn entry_for(draft: &mut Draft, field: Field, index: Option<usize>, line: usize) -> usize {
    if let Some(number) = index {
        return numbered(draft, number, line);
    }

    // A bare `desc` opens a cheat when the one being written already has a description; the
    // other bare keywords complete the cheat the last `desc` opened. Only the last bare cheat
    // can lack a description - a bare cheat is opened by a `desc`, or by another keyword when
    // there is no bare cheat yet - so it is the only one to look at.
    let open = draft
        .bare
        .filter(|&at| field != Field::Desc || draft.cheats[at].desc.is_none());
    if let Some(at) = open {
        return at;
    }
    draft.cheats.push(Cheat {
        line,
        ..Cheat::default()
    });
    draft.bare = Some(draft.cheats.len() - 1);
    draft.cheats.len() - 1
}

/// The cheat carrying `number`, opened by the line being read when the file has not named it
/// yet. The opening line is kept, so a refusal can point at the cheat and not only at the file.
fn numbered(draft: &mut Draft, number: usize, line: usize) -> usize {
    if let Some(&at) = draft.numbers.get(&number) {
        return at;
    }
    draft.cheats.push(Cheat {
        line,
        ..Cheat::default()
    });
    draft.numbers.insert(number, draft.cheats.len() - 1);
    draft.cheats.len() - 1
}

/// Close a record: build what the file left implicit and check what it declared.
fn finish(draft: Draft, fallback: Option<&str>) -> Result<Record, ParseError> {
    let mut draft = draft;
    if draft.name.is_empty() {
        draft.name = fallback.unwrap_or_default().to_owned();
    }
    let shown = if draft.name.is_empty() {
        "(unnamed record)".to_owned()
    } else {
        draft.name.clone()
    };
    if draft.hash.is_none() && draft.name.is_empty() {
        return Err(ParseError::MissingKey(shown));
    }

    let mut entries = Vec::with_capacity(draft.cheats.len());
    for cheat in draft.cheats {
        let line = cheat.line;
        let desc = cheat.desc.unwrap_or_default();
        let mut codes = cheat.codes;
        let mut note = cheat.note;
        if codes.is_empty() && note.is_none() {
            // No `code` string: the cheat is either the address and byte the file spells out,
            // or a note — shipped databases use a described entry with no code as a heading or
            // a comment inside the list, and `cheats = N` counts it.
            match (cheat.address, cheat.value) {
                (Some(addr), Some(value)) => match cheat.retro.unapplied(line) {
                    None => codes.push(Code {
                        addr,
                        value,
                        compare: None,
                    }),
                    Some(reason) if !desc.is_empty() => note = Some(reason),
                    Some(_) => {
                        return Err(ParseError::EmptyCode(line, "(unnamed cheat)".to_owned()));
                    }
                },
                // Half a pair is not a code: an address with no value must not become a write
                // of whatever the missing half would have been.
                (Some(_), None) | (None, Some(_)) if !desc.is_empty() => {
                    note = Some(format!(
                        "line {line}: the cheat gives only half of its address and value"
                    ));
                }
                (Some(_), None) | (None, Some(_)) => {
                    return Err(ParseError::EmptyCode(line, "(unnamed cheat)".to_owned()));
                }
                (None, None) if !desc.is_empty() => {}
                (None, None) => {
                    return Err(ParseError::EmptyCode(line, "(unnamed cheat)".to_owned()));
                }
            }
        }
        entries.push(Entry {
            desc,
            codes,
            enabled: cheat.enabled,
            note,
        });
    }

    match draft.declared {
        Some(declared) if declared != entries.len() => {
            return Err(ParseError::CountMismatch(shown, declared, entries.len()));
        }
        _ => {}
    }

    Ok(Record {
        hash: draft.hash,
        name: draft.name,
        entries,
    })
}

/// Every code in a `cheatN_code` value: Game Genie strings and raw `AAAA:VV` codes joined by
/// `+`, each decoded on its own.
fn decode_codes(number: usize, line: &str, text: &str) -> Result<Vec<Code>, ParseError> {
    let mut codes = Vec::new();
    for part in text.split('+') {
        let part = part.trim();
        if part.is_empty() {
            return Err(ParseError::BadCode(
                number,
                line.to_owned(),
                GenieError::BadLength,
            ));
        }
        if let Some(code) = parse_raw(part) {
            codes.push(code);
            continue;
        }
        let code = decode(part).map_err(|why| ParseError::BadCode(number, line.to_owned(), why))?;
        codes.push(code);
    }
    Ok(codes)
}

/// `AAAA:VV` as written in a database: a 16-bit address and the byte the game must see there,
/// both hex. A Game Genie string never contains `:`, so the two cannot be confused.
fn parse_raw(text: &str) -> Option<Code> {
    let (addr, value) = text.split_once(':')?;
    if addr.len() != 4 || value.len() != 2 {
        return None;
    }
    Some(Code {
        addr: u16::from_str_radix(addr, 16).ok()?,
        value: u8::from_str_radix(value, 16).ok()?,
        compare: None,
    })
}

/// `hash =` as 16 bytes, printed as 32 hex characters in either case, quoted or not.
fn parse_hash(value: &str) -> Option<[u8; 16]> {
    let text = unquote(value);
    let bytes = text.as_bytes();
    if bytes.len() != 32 || !bytes.iter().all(u8::is_ascii_hexdigit) {
        return None;
    }
    let mut hash = [0u8; 16];
    for (i, pair) in bytes.as_chunks::<2>().0.iter().enumerate() {
        hash[i] = (hex_digit(pair[0])? << 4) | hex_digit(pair[1])?;
    }
    Some(hash)
}

/// One hex digit, either case.
fn hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// A number in the base C's `strtoul` with base 0 picks — `0x` hex, a leading `0` octal,
/// decimal otherwise — which is the call RetroArch parses `address` and `value` with.
fn parse_number(value: &str) -> Option<u64> {
    let text = value.trim();
    if let Some(rest) = text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
        return u64::from_str_radix(rest, 16).ok();
    }
    if text.len() > 1 && text.starts_with('0') {
        return u64::from_str_radix(&text[1..], 8).ok();
    }
    text.parse::<u64>().ok()
}

/// `enable =` as a bool, in either case and in RetroArch's `1`/`0` spelling too.
fn parse_enable(value: &str) -> Option<bool> {
    match unquote(value).to_ascii_lowercase().as_str() {
        "true" | "1" => Some(true),
        "false" | "0" => Some(false),
        _ => None,
    }
}

/// A value with the database's optional quotes removed.
fn unquote(value: &str) -> String {
    let text = value.trim();
    match text
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
    {
        Some(inner) => inner.to_owned(),
        None => text.to_owned(),
    }
}

/// Whether the text is Mesen's XML notation rather than this one.
fn is_xml(text: &str) -> bool {
    text.strip_prefix('\u{feff}')
        .unwrap_or(text)
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with('#'))
        .is_some_and(|line| line.starts_with('<'))
}

/// What applying a cheat list does: which codes go to the Game Genie, which become RAM freezes,
/// and which are left alone and why. Built by [`apply_plan`]; nothing here touches a console.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ApplyPlan {
    /// Codes at `$8000..=$FFFF`, for the Game Genie read patch.
    pub genie: Vec<Code>,
    /// `(address, value)` pairs at `$0000..=$1FFF`, written to RAM each frame.
    pub freezes: Vec<(u16, u8)>,
    /// `(description, reason)` for each enabled cheat or code the caller will not apply.
    pub skipped: Vec<(String, String)>,
}

/// Sort the codes of the **enabled** entries into an [`ApplyPlan`]. Disabled entries are left
/// out entirely. A code at `$8000` or above is a Game Genie code, one at `$0000..=$1FFF` is a RAM
/// freeze, and anything else is skipped: `$2000..=$5FFF` (PPU/APU registers, open bus) and
/// `$6000..=$7FFF` (cartridge RAM, which is not frozen).
/// A freeze that carries a compare byte is refused, because a freeze has no read to compare
/// against and guessing a meaning would be wrong. An enabled note is reported as skipped with
/// the reason it carries.
pub fn apply_plan(entries: &[Entry]) -> ApplyPlan {
    let mut plan = ApplyPlan::default();
    for entry in entries.iter().filter(|entry| entry.enabled) {
        if entry.codes.is_empty() {
            let reason = entry.note.clone().unwrap_or_else(|| "no code".to_string());
            plan.skipped.push((entry.desc.clone(), reason));
            continue;
        }
        for code in &entry.codes {
            match code.addr {
                0x8000..=0xFFFF => plan.genie.push(*code),
                0x0000..=0x1FFF => match code.compare {
                    None => plan.freezes.push((code.addr, code.value)),
                    Some(compare) => plan.skipped.push((
                        entry.desc.clone(),
                        format!(
                            "RAM freeze at ${:04X} has a compare byte ${compare:02X}; not applied",
                            code.addr
                        ),
                    )),
                },
                0x6000..=0x7FFF => plan.skipped.push((
                    entry.desc.clone(),
                    format!(
                        "address ${:04X} is cartridge RAM; cartridge RAM codes are not supported",
                        code.addr
                    ),
                )),
                addr => plan.skipped.push((
                    entry.desc.clone(),
                    format!("address ${addr:04X} is outside RAM and cartridge space"),
                )),
            }
        }
    }
    plan
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A named record, for tests that only need one.
    fn one(cheat_lines: &str) -> Vec<Record> {
        parse(&format!("name = Fixture\n{cheat_lines}")).unwrap()
    }

    /// Parse a fixture and return why it failed, for the error-path tests.
    fn failing(cheat_lines: &str) -> ParseError {
        parse(&format!("name = Fixture\n{cheat_lines}")).unwrap_err()
    }

    /// The codes of the first cheat, for tests that only care about the decode.
    fn codes(cheat_lines: &str) -> Vec<Code> {
        one(cheat_lines).remove(0).entries.remove(0).codes
    }

    /// A fresh directory for a load test, unique to the tag and process.
    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("nes-cheatdb-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn the_sample_database_parses() {
        let records = parse(SAMPLE_DB).unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].name, "Super Mario Bros.");
        assert_eq!(records[1].name, "1942");
        assert_eq!(records[0].entries.len(), 3);
        assert_eq!(records[1].entries.len(), 2);
    }

    #[test]
    fn the_sample_records_are_keyed_the_way_the_file_says() {
        let records = parse(SAMPLE_DB).unwrap();
        // The first record names the RFC 1321 `abc` vector; the second has no hash line.
        assert_eq!(records[0].hash, Some(md5(b"abc")));
        assert_eq!(records[1].hash, None);
    }

    #[test]
    fn game_genie_codes_decode_through_the_genie_module() {
        // SXIOPO is the code genie.rs pins to $91D9:$AD from nesdev.org's nesgg.txt; this
        // module must agree with the decoder rather than shuffle the bits a second time.
        assert_eq!(
            codes("cheat0_desc = \"Infinite lives\"\ncheat0_code = \"SXIOPO\"\n"),
            vec![Code {
                addr: 0x91D9,
                value: 0xAD,
                compare: None
            }]
        );

        // SLXPLOVS is the 8-letter example, which keeps its compare byte.
        assert_eq!(
            codes("cheat0_code = \"SLXPLOVS\"\n"),
            vec![Code {
                addr: 0x9123,
                value: 0xBD,
                compare: Some(0xDE)
            }]
        );
    }

    #[test]
    fn a_cheat_may_carry_several_codes_joined_by_plus() {
        // `cheat0_code = "OZOYUEPX+OZVULSPX"` is how libretro-database writes a cheat that
        // needs two substitutions; both are applied, in the order the file lists them.
        let decoded = codes("cheat0_code = \"OZOYUEPX+OZVULSPX\"\n");
        assert_eq!(
            decoded,
            vec![decode("OZOYUEPX").unwrap(), decode("OZVULSPX").unwrap()]
        );
    }

    #[test]
    fn raw_address_codes_are_read_as_written() {
        // `0025:63` is value $63 at RAM address $0025, and `0595:4F` the same onto $0595.
        assert_eq!(
            codes("cheat0_code = \"0025:63\"\n"),
            vec![Code {
                addr: 0x0025,
                value: 0x63,
                compare: None
            }]
        );
        // Lower-case hex is the same code.
        assert_eq!(
            codes("cheat0_code = \"0595:4f\"\n"),
            vec![Code {
                addr: 0x0595,
                value: 0x4F,
                compare: None
            }]
        );
    }

    #[test]
    fn genie_and_raw_codes_may_share_one_cheat() {
        let decoded = codes("cheat0_code = \"SXIOPO+0025:63\"\n");
        assert_eq!(decoded.len(), 2);
        assert_eq!(decoded[0].addr, 0x91D9);
        assert_eq!(decoded[1].addr, 0x0025);
    }

    #[test]
    fn the_address_and_value_spelling_is_a_raw_code() {
        // The shipped file that uses this spelling writes an empty code and gives the address
        // and value separately, beside the three RetroArch keys that make RetroArch apply them.
        let records = one("cheat0_desc = \"Infinite health\"\ncheat0_code = \"\"\n\
             cheat0_address = \"820\"\ncheat0_value = \"4\"\n\
             cheat0_handler = \"1\"\ncheat0_cheat_type = \"1\"\ncheat0_memory_search_size = \"3\"\n");
        assert_eq!(records[0].entries[0].codes.len(), 1);
        // Base 0, as strtoul reads it: 820 is decimal.
        assert_eq!(
            records[0].entries[0].codes[0],
            Code {
                addr: 820,
                value: 4,
                compare: None
            }
        );
    }

    #[test]
    fn an_address_and_value_cheat_may_spell_its_numbers_in_any_base() {
        let hex = codes(
            "cheat0_address = \"0x334\"\ncheat0_value = \"0x04\"\ncheat0_handler = \"1\"\n\
             cheat0_cheat_type = \"1\"\ncheat0_memory_search_size = \"3\"\n",
        );
        assert_eq!(hex[0].addr, 0x334);
        assert_eq!(hex[0].value, 0x04);
        // A leading zero is octal, which is what strtoul with base 0 does with 0777.
        let octal = codes(
            "cheat0_address = \"0777\"\ncheat0_value = \"010\"\ncheat0_handler = \"1\"\n\
             cheat0_cheat_type = \"1\"\ncheat0_memory_search_size = \"3\"\n",
        );
        assert_eq!(octal[0].addr, 0o777);
        assert_eq!(octal[0].value, 8);
    }

    #[test]
    fn cheats_load_switched_off_unless_the_file_says_otherwise() {
        let records = one(
            "cheat0_desc = \"off by omission\"\ncheat0_code = \"SXIOPO\"\n\
             cheat1_desc = \"on\"\ncheat1_code = \"SXIOPO\"\ncheat1_enable = true\n\
             cheat2_desc = \"one is on too\"\ncheat2_code = \"SXIOPO\"\ncheat2_enable = \"1\"\n\
             cheat3_desc = \"and zero is off\"\ncheat3_code = \"SXIOPO\"\ncheat3_enable = \"0\"\n",
        );
        let on: Vec<bool> = records[0]
            .entries
            .iter()
            .map(|entry| entry.enabled)
            .collect();
        assert_eq!(on, vec![false, true, true, false]);
    }

    #[test]
    fn quoted_values_keep_their_punctuation() {
        let records = one(
            "cheat0_desc = \"5 lives: press START\"\ncheat0_code = \"SXIOPO\"\n\
             cheat1_desc = unquoted description\ncheat1_code = \"SXIOPO\"\n",
        );
        assert_eq!(records[0].entries[0].desc, "5 lives: press START");
        assert_eq!(records[0].entries[1].desc, "unquoted description");
    }

    #[test]
    fn keywords_are_case_insensitive_and_spacing_around_the_equals_is_free() {
        let records = one(
            "CHEAT0_CODE   = \"SXIOPO\"\ncheat0_DESC = Written In Caps\nCheat0_Enable = TRUE\n",
        );
        assert_eq!(records[0].entries.len(), 1);
        assert_eq!(records[0].entries[0].desc, "Written In Caps");
        assert!(records[0].entries[0].enabled);
        assert_eq!(records[0].entries[0].codes[0].addr, 0x91D9);

        let hash = parse("NaMe = Mixed\nHASH = 900150983CD24FB0D6963F7D28E17F72\n").unwrap();
        assert_eq!(hash[0].hash, Some(md5(b"abc")));
    }

    #[test]
    fn comments_and_blank_lines_are_skipped() {
        let records = parse(
            "# a database with nothing but comments and one cheat\n\n\
             name = Commented\n\n# and a cheat\n\ncheat0_desc = only\ncheat0_code = \"SXIOPO\"\n",
        )
        .unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].entries.len(), 1);
    }

    #[test]
    fn a_record_may_be_keyed_by_name_alone() {
        // A shipped .cht carries no hash line, so the name is the only key it has.
        let records = parse("name = 8 Eyes (USA)\ncheat0_code = \"0595:4F\"\n").unwrap();
        assert_eq!(records[0].hash, None);
        assert_eq!(records[0].name, "8 Eyes (USA)");
        assert_eq!(
            lookup_name(&records, "8 eyes (usa)").unwrap().entries.len(),
            1
        );
        assert!(lookup(&records, md5(b"abc")).is_none());
    }

    #[test]
    fn a_record_with_neither_hash_nor_name_is_rejected() {
        let err = parse("cheat0_code = \"SXIOPO\"\n").unwrap_err();
        assert_eq!(err, ParseError::MissingKey("(unnamed record)".to_owned()));
        assert_eq!(
            err.to_string(),
            "the record \"(unnamed record)\" has no ROM hash and no name"
        );
    }

    #[test]
    fn lookup_finds_the_record_for_the_rom_data() {
        let records = parse(SAMPLE_DB).unwrap();
        let found = lookup(&records, rom_digest(b"abc")).unwrap();
        assert_eq!(found.name, "Super Mario Bros.");
        assert_eq!(found.entries.len(), 3);
    }

    #[test]
    fn lookup_finds_nothing_for_an_unlisted_rom() {
        let records = parse(SAMPLE_DB).unwrap();
        assert!(lookup(&records, rom_digest(b"another rom")).is_none());
        // ...and its name does not stand in for the hash the record carries.
        assert!(lookup_name(&records, "Super Mario Bros.").is_none());
    }

    #[test]
    fn one_file_may_hold_several_records() {
        let records = parse(SAMPLE_DB).unwrap();
        let names: Vec<&str> = records.iter().map(|record| record.name.as_str()).collect();
        assert_eq!(names, vec!["Super Mario Bros.", "1942"]);
    }

    #[test]
    fn cheats_may_be_numbered_out_of_order_and_with_gaps() {
        let records = one("cheat2_desc = \"listed first\"\ncheat2_code = \"SXIOPO\"\n\
             cheat0_desc = \"listed second\"\ncheat0_code = \"AAAAAE\"\n");
        let descs: Vec<&str> = records[0]
            .entries
            .iter()
            .map(|entry| entry.desc.as_str())
            .collect();
        assert_eq!(descs, vec!["listed first", "listed second"]);
    }

    #[test]
    fn a_second_bare_desc_starts_the_next_cheat() {
        let records = one("desc = one\ncode = \"SXIOPO\"\ndesc = two\ncode = \"AAAAAE\"\n");
        assert_eq!(records[0].entries.len(), 2);
        assert_eq!(records[0].entries[1].desc, "two");
        assert_eq!(records[0].entries[1].codes[0].addr, 0x8000);
    }

    #[test]
    fn the_declared_cheat_count_must_match_the_entries() {
        let err =
            parse("name = Miscounted\ncheats = 3\ncheat0_desc = one\ncheat0_code = \"SXIOPO\"\n")
                .unwrap_err();
        assert_eq!(
            err,
            ParseError::CountMismatch("Miscounted".to_owned(), 3, 1)
        );
        assert_eq!(
            err.to_string(),
            "the record \"Miscounted\" declares 3 cheats but has 1"
        );

        // The count the sample declares is the count it has.
        assert!(parse(SAMPLE_DB).is_ok());
    }

    #[test]
    fn a_hash_that_is_not_32_hex_characters_is_rejected() {
        for value in [
            "zz",
            "900150983cd24fb0d6963f7d28e17f7",
            "900150983cd24fb0d6963f7d28e17f721",
            "gg0150983cd24fb0d6963f7d28e17f72",
        ] {
            let err = parse(&format!("hash = {value}\ncheat0_code = \"SXIOPO\"\n")).unwrap_err();
            assert_eq!(
                err,
                ParseError::BadHash(1, value.to_owned()),
                "hash {value:?}"
            );
        }

        // A quoted hash is the same hash, quotes and all.
        let quoted =
            parse("hash = \"900150983cd24fb0d6963f7d28e17f72\"\ncheat0_code = \"SXIOPO\"\n")
                .unwrap();
        assert_eq!(quoted[0].hash, Some(md5(b"abc")));
    }

    #[test]
    fn a_bad_code_skips_its_cheat_and_keeps_the_file() {
        // '?' is outside APZLGITYEOXUKSVN, which genie.rs reports as BadLetter. The cheat stays in
        // the record with the decoder's reason as its note, and the cheat beside it still reads.
        let records = parse(
            "name = Bad code
cheat0_desc = lives
cheat0_enable = true
cheat0_code = \"SXIO?O\"
cheat1_desc = ammo
cheat1_enable = true
cheat1_code = \"AAAAAE\"
",
        )
        .unwrap();
        let entries = &records[0].entries;
        assert_eq!(entries.len(), 2);
        assert!(entries[0].codes.is_empty());
        let note = entries[0].note.as_deref().expect("the bad cheat says why");
        assert!(
            note.starts_with("line 4:"),
            "the report names the line: {note}"
        );
        assert_eq!(entries[1].codes[0].addr, 0x8000);

        let plan = apply_plan(entries);
        assert_eq!(plan.genie.len(), 1, "the good cheat is applied");
        assert_eq!(plan.skipped.len(), 1, "the bad one is counted as skipped");
        assert_eq!(plan.skipped[0].0, "lives");
        assert!(plan.skipped[0].1.starts_with("line 4:"));
    }

    #[test]
    fn a_bad_part_of_a_code_group_skips_the_cheat() {
        // The same goes for one part of a `+` group, and a part of the wrong length.
        let records = parse(
            "name = Bad group
cheat0_code = \"SXIOPO+SXIO?O\"
",
        )
        .unwrap();
        assert!(records[0].entries[0].codes.is_empty());
        let note = records[0].entries[0].note.as_deref().unwrap();
        assert!(note.starts_with("line 2:"), "{note}");

        let records = parse(
            "name = Short
cheat0_code = \"SXIOP\"
",
        )
        .unwrap();
        let note = records[0].entries[0].note.as_deref().unwrap();
        assert!(note.starts_with("line 2:"), "{note}");
    }

    #[test]
    fn cartridge_ram_codes_are_named_as_cartridge_ram() {
        let records = one("cheat0_desc = save
cheat0_enable = true
cheat0_code = \"6000:01\"
");
        let plan = apply_plan(&records[0].entries);
        assert!(plan.genie.is_empty() && plan.freezes.is_empty());
        let reason = &plan.skipped[0].1;
        assert!(reason.contains("cartridge RAM"), "{reason}");
        assert!(!reason.contains("outside"), "{reason}");
    }

    #[test]
    fn a_described_entry_with_no_code_is_a_note() {
        // Shipped databases put headings and comments in the cheat list as a described entry
        // with an empty code, and `cheats = N` counts them.
        let records = one(
            "cheat0_desc = \"Note\"\ncheat0_code = \"\"\ncheat0_enable = false\n\
             cheat1_desc = \"Infinite lives\"\ncheat1_code = \"SXIOPO\"\n\
             cheat1_enable = false\n",
        );
        assert_eq!(records[0].entries.len(), 2);
        assert_eq!(records[0].entries[0].desc, "Note");
        assert!(records[0].entries[0].codes.is_empty());
        assert_eq!(records[0].entries[1].codes.len(), 1);

        // An entry with neither a description nor a code is a mistake, not a note.
        let err = parse("name = Empty\ncheat0_code = \"\"\n").unwrap_err();
        assert_eq!(err, ParseError::EmptyCode(2, "(unnamed cheat)".to_owned()));
        assert_eq!(
            err.to_string(),
            "line 2: the cheat \"(unnamed cheat)\" has no code"
        );
    }

    #[test]
    fn a_quoted_cheat_count_is_the_same_count() {
        // Some shipped files write `cheats = "2"`.
        let quoted = parse(
            "name = Quoted\ncheats = \"2\"\ncheat0_code = \"SXIOPO\"\n\
                            cheat1_code = \"SXIOPO\"\n",
        )
        .unwrap();
        assert_eq!(quoted[0].entries.len(), 2);

        let unquoted = parse(
            "name = Quoted\ncheats = 2\ncheat0_code = \"SXIOPO\"\n\
                              cheat1_code = \"SXIOPO\"\n",
        )
        .unwrap();
        assert_eq!(quoted, unquoted);

        let err = parse("name = Bad\ncheats = two\ncheat0_code = \"SXIOPO\"\n").unwrap_err();
        assert_eq!(err, ParseError::BadNumber(2, "two".to_owned()));
    }

    #[test]
    fn a_value_that_is_not_a_byte_is_rejected() {
        assert_eq!(
            failing("cheat0_address = \"820\"\ncheat0_value = \"256\"\n"),
            ParseError::BadValue(3, "256".to_owned())
        );
        // An address that is not 16 bits is the same mistake.
        assert_eq!(
            failing("cheat0_address = \"0x10000\"\ncheat0_value = \"1\"\n"),
            ParseError::BadValue(2, "0x10000".to_owned())
        );
    }

    #[test]
    fn a_keyword_with_no_value_is_rejected() {
        let err = parse("name = Empty\ncheat0_code =\n").unwrap_err();
        assert_eq!(err, ParseError::NoValue(2, "cheat0_code =".to_owned()));
        assert_eq!(err.to_string(), "line 2 has no value: \"cheat0_code =\"");
    }

    #[test]
    fn an_unknown_keyword_is_reported_but_cheat_metadata_is_skipped() {
        let err = parse("name = Oops\ncheat_cod = \"SXIOPO\"\n").unwrap_err();
        assert_eq!(
            err,
            ParseError::UnknownKeyword(2, "cheat_cod = \"SXIOPO\"".to_owned())
        );

        // The metadata libretro-database and RetroArch write is not a mistake.
        let records = one(
            "cheat0_desc = \"Infinite health\"\ncheat0_code = \"SXIOPO\"\n\
             cheat0_cheat_type = \"1\"\ncheat0_handler = \"1\"\n\
             cheat0_address_bit_position = \"0\"\ncheat0_big_endian = \"false\"\n\
             cheat0_memory_search_size = \"3\"\ncheat0_rumble_port = \"0\"\n\
             cheat0_repeat_count = \"1\"\nauthor = somebody\nsortname = fixture\n",
        );
        assert_eq!(records[0].entries.len(), 1);
    }

    #[test]
    fn xml_cheat_files_are_refused_with_the_reason() {
        let err = parse(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
             <cheats version=\"1.0\">\n\
             <cheat enabled=\"0\"><genie>AVXNXPVG</genie></cheat>\n\
             </cheats>\n",
        )
        .unwrap_err();
        assert_eq!(err, ParseError::NotCht);
        assert_eq!(
            err.to_string(),
            "this is an XML cheat file; only the key = value .cht format is read"
        );
    }

    #[test]
    fn a_leading_byte_order_mark_is_ignored() {
        // Shipped files carry a BOM; without this the first keyword starts with U+FEFF and the
        // whole file is one unknown-keyword error.
        let records = parse("\u{feff}name = Bom\ncheat0_code = \"SXIOPO\"\n").unwrap();
        assert_eq!(records[0].name, "Bom");
        assert_eq!(records[0].entries.len(), 1);
    }

    #[test]
    fn bytes_that_are_not_utf8_still_parse() {
        // A description in Windows-1252 (0xE9 is é): the bytes are not UTF-8, and reading one
        // character per byte keeps the ASCII codes intact.
        let bytes = b"name = Latin\ncheat0_desc = Pok\xE9mon\ncheat0_code = \"SXIOPO\"\n\
                      cheat1_code = \"0025:63\"\n";
        let records = parse_bytes(bytes).unwrap();
        assert_eq!(records[0].entries.len(), 2);
        assert_eq!(records[0].entries[0].codes[0].addr, 0x91D9);
        assert_eq!(records[0].entries[1].codes[0].addr, 0x0025);
    }

    #[test]
    fn an_empty_database_is_no_records() {
        assert!(parse("").unwrap().is_empty());
        assert!(parse("# only a comment\n\n").unwrap().is_empty());
    }

    #[test]
    fn cheats_dir_and_db_path_sit_next_to_the_rom() {
        assert_eq!(
            cheats_dir(Path::new("games/zelda.nes")),
            PathBuf::from("games/cheats")
        );
        assert_eq!(
            db_path(Path::new("games/zelda.nes")),
            PathBuf::from("games/cheats/zelda.cht")
        );
        // A bare filename has no parent directory to speak of.
        assert_eq!(cheats_dir(Path::new("zelda.nes")), PathBuf::from("cheats"));
    }

    #[test]
    fn load_file_names_a_nameless_record_after_the_file() {
        let dir = scratch("load-file");
        let path = dir.join("8 Eyes (USA).cht");
        std::fs::write(&path, "cheats = 1\ncheat0_code = \"0595:4F\"\n").unwrap();

        let records = load_file(&path).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].name, "8 Eyes (USA)");
        assert_eq!(records[0].entries.len(), 1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_database_file_is_a_read_error() {
        let dir = scratch("missing");
        let path = dir.join("absent.cht");
        let err = load_file(&path).unwrap_err();
        assert!(matches!(err, LoadError::Read(..)), "{err:?}");
        assert!(
            err.to_string().contains("absent.cht"),
            "the report names the file: {err}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_dir_reads_every_cht_file_and_skips_the_rest() {
        let dir = scratch("load-dir");
        std::fs::write(
            dir.join("a.cht"),
            "name = A\nhash = 900150983cd24fb0d6963f7d28e17f72\ncheat0_code = \"SXIOPO\"\n",
        )
        .unwrap();
        std::fs::write(dir.join("b.cht"), "cheat0_code = \"0595:4F\"\n").unwrap();
        // The XML notation in the same folder must not stop the load, nor must a stray file.
        std::fs::write(dir.join("c.xml"), "<cheats version=\"1.0\">\n").unwrap();
        std::fs::write(dir.join("notes.txt"), "not a database\n").unwrap();

        let loaded = load_dir(&dir).unwrap();
        assert_eq!(loaded.files, 2);
        assert_eq!(loaded.loaded(), 2);
        assert_eq!(loaded.refused(), 0);
        assert!(loaded.failures.is_empty());
        let records = loaded.records;
        assert_eq!(records.len(), 2);
        // Filename order: a.cht keeps its own name, b.cht takes its stem.
        assert_eq!(records[0].name, "A");
        assert_eq!(records[0].hash, Some(md5(b"abc")));
        assert_eq!(records[1].name, "b");
        assert_eq!(records[1].entries[0].codes[0].addr, 0x0595);

        // A directory that is not there means no cheats yet, not an error.
        assert!(load_dir(&dir.join("absent")).unwrap().records.is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_bad_file_in_a_directory_is_reported_and_the_good_ones_still_load() {
        // 83 of the 2262 shipped .cht files are refused, so a directory load must survive one
        // and say which file it was.
        let dir = scratch("load-dir-bad");
        std::fs::write(dir.join("good.cht"), "cheat0_code = \"SXIOPO\"\n").unwrap();
        std::fs::write(dir.join("bad.cht"), "nonsense line\n").unwrap();

        let loaded = load_dir(&dir).unwrap();
        assert_eq!(loaded.files, 2);
        assert_eq!(loaded.loaded(), 1);
        assert_eq!(loaded.refused(), 1);
        assert_eq!(loaded.records.len(), 1);
        assert_eq!(loaded.records[0].name, "good");
        assert_eq!(
            loaded.failures,
            vec![LoadError::Malformed(
                dir.join("bad.cht"),
                ParseError::UnknownKeyword(1, "nonsense line".to_owned())
            )]
        );
        let report = loaded.failures[0].to_string();
        assert!(
            report.contains("bad.cht") && report.contains("line 1"),
            "the report names the file and the line: {report}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn errors_display_and_are_std_errors() {
        let boxed: Box<dyn std::error::Error> = Box::new(ParseError::NotCht);
        assert!(boxed.to_string().starts_with("this is an XML cheat file"));

        let boxed: Box<dyn std::error::Error> =
            Box::new(LoadError::Read(PathBuf::from("a.cht"), "boom".to_owned()));
        assert_eq!(boxed.to_string(), "a.cht: boom");
    }

    /// The three RetroArch keys that make an `address`/`value` cheat one byte set at one
    /// address, in the spelling libretro-database writes them.
    const RETRO_KEYS: &str = "cheat0_handler = \"1\"\n\
                              cheat0_cheat_type = \"1\"\n\
                              cheat0_memory_search_size = \"3\"\n";

    #[test]
    fn a_disabled_cheat_type_imports_as_a_note_with_no_code() {
        // 102 of the shipped cheats are `cheat_type = 0` placeholders — the `(Rumbles)` files
        // are all this — and importing them as RAM writes makes a rumble trigger change the
        // game. The description stays, the code does not.
        let records = parse(
            "name = Adventures of Lolo (Rumbles)\n\
             cheats = 1\n\
             cheat0_desc = \"lolo death (Rumble)\"\n\
             cheat0_code = \"\"\n\
             cheat0_address = \"103\"\n\
             cheat0_value = \"1\"\n\
             cheat0_handler = \"1\"\n\
             cheat0_cheat_type = \"0\"\n\
             cheat0_memory_search_size = \"3\"\n\
             cheat0_address_bit_position = \"0\"\n\
             cheat0_enable = \"1\"\n",
        )
        .unwrap();
        assert_eq!(records[0].entries.len(), 1);
        let entry = &records[0].entries[0];
        assert_eq!(entry.desc, "lolo death (Rumble)");
        assert!(entry.codes.is_empty(), "{entry:?}");
        assert!(entry.enabled, "the switch the file gave is kept");
        let note = entry.note.as_deref().expect("the refusal is reported");
        assert!(
            note.starts_with("line 8:"),
            "the note names the line: {note}"
        );
        assert!(note.contains("cheat_type 0"), "{note}");
    }

    #[test]
    fn a_retro_combo_that_writes_one_byte_imports_normally() {
        // Action 52 ships cheats this way: the empty code, the address and value, and the
        // three keys that make RetroArch write one byte there.
        let records = one(&format!(
            "cheat0_desc = \"Infinite health\"\n\
             cheat0_code = \"\"\n\
             cheat0_address = \"820\"\n\
             cheat0_value = \"4\"\n\
             {RETRO_KEYS}\
             cheat0_address_bit_position = \"0\"\n\
             cheat0_repeat_count = \"1\"\n"
        ));
        let entry = &records[0].entries[0];
        assert_eq!(
            entry.codes,
            vec![Code {
                addr: 820,
                value: 4,
                compare: None
            }]
        );
        assert_eq!(entry.note, None);

        // BurgerTime and Micro Mages write `address_bit_position = 255`: at one byte of width
        // RetroArch's write is the whole byte, so 255 is the field that says so.
        let records = one(&format!(
            "cheat0_desc = \"max lives\"\n\
             cheat0_address = \"90\"\n\
             cheat0_value = \"255\"\n\
             {RETRO_KEYS}\
             cheat0_address_bit_position = \"255\"\n"
        ));
        assert_eq!(records[0].entries[0].codes[0].addr, 90);
        assert_eq!(records[0].entries[0].note, None);
    }

    #[test]
    fn an_address_value_cheat_without_its_retro_keys_is_not_a_write() {
        // RetroArch reaches an address and value only through handler 1, and only as its
        // cheat_type says; a file that names neither gets no invented write.
        let bare = |extra: &str| {
            let text = format!(
                "cheat0_desc = \"Invented\"\ncheat0_address = \"820\"\ncheat0_value = \"4\"\n{extra}"
            );
            one(&text).remove(0).entries.remove(0)
        };

        let entry = bare("");
        assert!(entry.codes.is_empty(), "{entry:?}");
        assert!(entry.note.as_deref().unwrap().contains("no cheat_type"));

        let entry = bare("cheat0_cheat_type = \"1\"\n");
        assert!(entry.codes.is_empty(), "{entry:?}");
        assert!(entry.note.as_deref().unwrap().contains("no handler"));

        let entry = bare("cheat0_cheat_type = \"1\"\ncheat0_handler = \"1\"\n");
        assert!(entry.codes.is_empty(), "{entry:?}");
        assert!(
            entry
                .note
                .as_deref()
                .unwrap()
                .contains("no memory_search_size")
        );

        // Half a pair is not a code either.
        let records = one("cheat0_desc = \"Half\"\ncheat0_address = \"820\"\n");
        let entry = &records[0].entries[0];
        assert!(entry.codes.is_empty(), "{entry:?}");
        assert!(entry.note.as_deref().unwrap().contains("only half"));
    }

    #[test]
    fn every_retro_key_that_stops_the_code_says_why_and_where() {
        // One key away from the combo above, each of these writes something a single byte
        // cannot express. The entry keeps its description, loses its code, and says which line
        // refused it.
        let cases = [
            ("cheat0_handler = \"0\"\n", "handler 0"),
            ("cheat0_handler = \"2\"\n", "handler 2"),
            ("cheat0_cheat_type = \"2\"\n", "cheat_type 2"),
            ("cheat0_cheat_type = \"3\"\n", "cheat_type 3"),
            ("cheat0_cheat_type = \"4\"\n", "run-next-if"),
            (
                "cheat0_memory_search_size = \"4\"\n",
                "memory_search_size 4",
            ),
            (
                "cheat0_memory_search_size = \"2\"\n",
                "memory_search_size 2",
            ),
            (
                "cheat0_address_bit_position = \"3\"\n",
                "address_bit_position 3",
            ),
            ("cheat0_repeat_count = \"2\"\n", "repeat_count 2"),
            ("cheat0_repeat_count = \"0\"\n", "repeat_count 0"),
        ];
        for (line, expected) in cases {
            let text = format!(
                "cheat0_desc = \"Fixture\"\n\
                 cheat0_address = \"820\"\n\
                 cheat0_value = \"4\"\n\
                 {RETRO_KEYS}\
                 {line}"
            );
            let entry = one(&text).remove(0).entries.remove(0);
            assert!(
                entry.codes.is_empty(),
                "{line:?} must not become a code: {entry:?}"
            );
            let note = entry.note.as_deref().unwrap_or_default();
            assert!(note.contains(expected), "{line:?}: {note}");
            assert!(note.starts_with("line "), "{line:?}: {note}");
            assert_eq!(entry.desc, "Fixture");
        }
    }

    #[test]
    fn a_hash_before_the_name_is_one_record() {
        // `hash` is this module's own extension, so the order of the two keys must not split
        // the record: a hashed half with no name and no cheats is unreachable by `lookup`.
        let records = parse(
            "hash = 900150983cd24fb0d6963f7d28e17f72\n\
             name = Hash first\n\
             cheat0_desc = \"one\"\n\
             cheat0_code = \"SXIOPO\"\n",
        )
        .unwrap();
        assert_eq!(records.len(), 1, "one record, not two: {records:?}");
        assert_eq!(records[0].hash, Some(md5(b"abc")));
        assert_eq!(records[0].name, "Hash first");
        assert_eq!(records[0].entries.len(), 1);
        assert_eq!(lookup(&records, md5(b"abc")).unwrap().entries.len(), 1);

        // The other order builds the same record.
        let other = parse(
            "name = Hash first\n\
             hash = 900150983cd24fb0d6963f7d28e17f72\n\
             cheat0_desc = \"one\"\n\
             cheat0_code = \"SXIOPO\"\n",
        )
        .unwrap();
        assert_eq!(records, other);

        // A second `name` or `hash` still starts the next record.
        let two =
            parse("name = One\ncheat0_code = \"SXIOPO\"\nname = Two\ncheat0_code = \"SXIOPO\"\n")
                .unwrap();
        assert_eq!(two.len(), 2);
        assert_eq!(two[1].name, "Two");
    }

    #[test]
    fn a_load_error_names_the_file_and_the_line() {
        let dir = scratch("load-file-bad");
        let path = dir.join("bad.cht");
        std::fs::write(&path, "name = Bad\ncheat0_code = \"\"\n").unwrap();

        let err = load_file(&path).unwrap_err();
        assert_eq!(
            err,
            LoadError::Malformed(
                path.clone(),
                ParseError::EmptyCode(2, "(unnamed cheat)".to_owned())
            )
        );
        assert_eq!(err.path(), path.as_path());
        let report = err.to_string();
        assert!(
            report.contains("bad.cht") && report.contains("line 2"),
            "the report names the file and the line: {report}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn every_sample_cheat_applies_somewhere() {
        // What the module produces has to be usable: every code in the sample substitutes at
        // its own address, and an 8-letter code honours its compare byte.
        let records = parse(SAMPLE_DB).unwrap();
        let entries: Vec<&Entry> = records.iter().flat_map(|record| &record.entries).collect();
        assert_eq!(entries.len(), 5);
        for entry in &entries {
            for code in &entry.codes {
                let current = code.compare.unwrap_or(0x00);
                assert_eq!(code.apply(code.addr, current), code.value, "{entry:?}");
            }
        }
    }

    /// A fixture `.cht` with a GG cheat, a RAM cheat, a `$6000` cheat and a disabled cheat.
    const PLAN_DB: &str = "name = Plan\n\
        cheat0_desc = \"GG\"\ncheat0_code = \"PASIOALE\"\ncheat0_enable = true\n\
        cheat1_desc = \"RAM\"\ncheat1_code = \"0025:63\"\ncheat1_enable = true\n\
        cheat2_desc = \"SRAM\"\ncheat2_code = \"6000:01\"\ncheat2_enable = true\n\
        cheat3_desc = \"Off\"\ncheat3_code = \"0030:09\"\ncheat3_enable = false\n";

    #[test]
    fn apply_plan_splits_the_fixture_one_one_one() {
        let entries = parse(PLAN_DB).unwrap().remove(0).entries;
        let plan = apply_plan(&entries);
        assert_eq!(plan.genie.len(), 1);
        assert!(plan.genie[0].addr >= 0x8000);
        assert_eq!(plan.freezes, vec![(0x0025, 0x63)]);
        assert_eq!(plan.skipped.len(), 1);
        assert_eq!(plan.skipped[0].0, "SRAM");
        // The disabled cheat appears nowhere.
        assert!(!plan.freezes.contains(&(0x0030, 0x09)));
        assert!(plan.skipped.iter().all(|(name, _)| name != "Off"));
    }

    #[test]
    fn apply_plan_refuses_a_freeze_with_a_compare_byte() {
        let code = Code {
            addr: 0x0100,
            value: 1,
            compare: Some(2),
        };
        let entry = Entry {
            desc: "Cmp".into(),
            codes: vec![code],
            enabled: true,
            note: None,
        };
        let plan = apply_plan(&[entry]);
        assert!(plan.freezes.is_empty() && plan.genie.is_empty());
        assert_eq!(plan.skipped.len(), 1);
        assert!(plan.skipped[0].1.contains("compare"));
    }

    #[test]
    fn apply_plan_boundaries_and_notes() {
        let at = |addr| Code {
            addr,
            value: 7,
            compare: None,
        };
        let entry = |desc: &str, codes, note| Entry {
            desc: desc.into(),
            codes,
            enabled: true,
            note,
        };
        let plan = apply_plan(&[
            entry(
                "a",
                vec![at(0x1FFF), at(0x2000), at(0x7FFF), at(0x8000)],
                None,
            ),
            entry("note", vec![], Some("refused line".into())),
        ]);
        assert_eq!(plan.freezes, vec![(0x1FFF, 7)]);
        assert_eq!(plan.genie, vec![at(0x8000)]);
        assert_eq!(plan.skipped.len(), 3);
        assert_eq!(
            plan.skipped[2],
            ("note".to_string(), "refused line".to_string())
        );
        assert_eq!(apply_plan(&[]), ApplyPlan::default());
    }
}
