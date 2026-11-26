//! tendy-core: the contract between the front end and every core. No emulation code lives here.

use std::any::Any;
use std::fmt;

pub mod pin;
pub mod state;

/// Which core runs a file. Game Boy, Color and Super Game Boy share `Gb`: one core runs all three,
/// and the model is a core setting chosen as today (SPEC §4).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SystemKind {
    /// `tendy-gb`: DMG, CGB, SGB.
    Gb,
    /// `tendy-gba`.
    Gba,
    /// `tendy-nes`: cartridges, FDS/QD disks, NSF.
    Nes,
    /// `tendy-snes`.
    Snes,
}

impl SystemKind {
    /// Every system, in menu order.
    pub const ALL: [SystemKind; 4] = [
        SystemKind::Gb,
        SystemKind::Gba,
        SystemKind::Nes,
        SystemKind::Snes,
    ];

    /// The four bytes a save-state container stores for this system (§2.7).
    pub const fn tag(self) -> [u8; 4] {
        match self {
            SystemKind::Gb => *b"GB  ",
            SystemKind::Gba => *b"GBA ",
            SystemKind::Nes => *b"NES ",
            SystemKind::Snes => *b"SNES",
        }
    }

    /// The system a container tag names, if any.
    pub fn from_tag(tag: [u8; 4]) -> Option<SystemKind> {
        SystemKind::ALL.into_iter().find(|s| s.tag() == tag)
    }

    /// Settings section, folder and keys-file name: `gb`, `gba`, `nes`, `snes`.
    pub const fn key(self) -> &'static str {
        match self {
            SystemKind::Gb => "gb",
            SystemKind::Gba => "gba",
            SystemKind::Nes => "nes",
            SystemKind::Snes => "snes",
        }
    }

    /// The name the user reads.
    pub const fn display_name(self) -> &'static str {
        match self {
            SystemKind::Gb => "Game Boy",
            SystemKind::Gba => "Game Boy Advance",
            SystemKind::Nes => "NES",
            SystemKind::Snes => "Super NES",
        }
    }

    /// The magic each source wrote at the start of its own state files, so states copied from the
    /// earlier apps still load (§2.7). Sources: gb `src/state.rs:17`, gb `src/gba/mod.rs:163`,
    /// nes `src/state.rs:13`, snes `src/state.rs:23`.
    pub const fn legacy_state_magic(self) -> [u8; 4] {
        match self {
            SystemKind::Gb => *b"GBST",
            SystemKind::Gba => *b"GBAG",
            SystemKind::Nes => *b"NESS",
            SystemKind::Snes => *b"SNST",
        }
    }
}

/// One finished picture as the front end draws it: RGBA8888 (alpha always 0xFF), row-major,
/// top-left first, `width * 4` bytes per row, no padding. This is the only pixel format that
/// crosses the boundary (SPEC §5). Each core converts from its own buffer (§2.4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Frame<'a> {
    /// Pixels per row.
    pub width: u32,
    /// Rows.
    pub height: u32,
    /// `width * height * 4` bytes.
    pub rgba: &'a [u8],
}

impl<'a> Frame<'a> {
    /// Bytes per pixel at the boundary.
    pub const BYTES_PER_PIXEL: usize = 4;

    /// A frame over `rgba`. The core sizes the buffer.
    pub const fn new(width: u32, height: u32, rgba: &'a [u8]) -> Frame<'a> {
        Frame {
            width,
            height,
            rgba,
        }
    }

    /// Whether `rgba` holds exactly `width * height` pixels.
    pub fn is_complete(&self) -> bool {
        self.rgba.len() == self.width as usize * self.height as usize * Self::BYTES_PER_PIXEL
    }
}

/// The logical pad (SPEC §5): D-pad, A B X Y, L R, Start, Select, one bit each. Each core maps
/// it to its own pad (§2.5).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Buttons(pub u16);

impl Buttons {
    /// Nothing pressed.
    pub const NONE: Buttons = Buttons(0);
    /// D-pad up.
    pub const UP: Buttons = Buttons(1 << 0);
    /// D-pad down.
    pub const DOWN: Buttons = Buttons(1 << 1);
    /// D-pad left.
    pub const LEFT: Buttons = Buttons(1 << 2);
    /// D-pad right.
    pub const RIGHT: Buttons = Buttons(1 << 3);
    /// A (right face button).
    pub const A: Buttons = Buttons(1 << 4);
    /// B (bottom face button).
    pub const B: Buttons = Buttons(1 << 5);
    /// X (top face button; SNES only).
    pub const X: Buttons = Buttons(1 << 6);
    /// Y (left face button; SNES only).
    pub const Y: Buttons = Buttons(1 << 7);
    /// Left shoulder (GBA, SNES).
    pub const L: Buttons = Buttons(1 << 8);
    /// Right shoulder (GBA, SNES).
    pub const R: Buttons = Buttons(1 << 9);
    /// Start.
    pub const START: Buttons = Buttons(1 << 10);
    /// Select.
    pub const SELECT: Buttons = Buttons(1 << 11);

    /// Every button with the name the keys file uses, in settings order.
    pub const ALL: [(Buttons, &'static str); 12] = [
        (Buttons::UP, "up"),
        (Buttons::DOWN, "down"),
        (Buttons::LEFT, "left"),
        (Buttons::RIGHT, "right"),
        (Buttons::A, "a"),
        (Buttons::B, "b"),
        (Buttons::X, "x"),
        (Buttons::Y, "y"),
        (Buttons::L, "l"),
        (Buttons::R, "r"),
        (Buttons::START, "start"),
        (Buttons::SELECT, "select"),
    ];

    /// Whether every bit of `other` is pressed.
    pub const fn contains(self, other: Buttons) -> bool {
        self.0 & other.0 == other.0
    }

    /// `self` with `other` pressed too.
    pub const fn with(self, other: Buttons) -> Buttons {
        Buttons(self.0 | other.0)
    }
}

/// A feature only some cores have. The menu shows a row only when the running core reports the
/// row's capability (§2.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Capability {
    /// Per-channel audio mute rows.
    ChannelMask = 0,
    /// Cheat codes, as one menu row (SPEC §6).
    Cheats = 1,
    /// Super Game Boy border around the picture.
    SgbBorder = 2,
    /// Super Game Boy sound through the SGB cartridge's own sound program.
    SgbSound = 3,
    /// A user-supplied boot ROM.
    BootRom = 4,
    /// Selectable built-in palettes or palette files (GB DMG shades, NES palettes).
    Palettes = 5,
    /// The Game Boy LCD filter pack.
    LcdFilter = 6,
    /// Link cable over a local socket.
    Link = 7,
    /// Tilt sensor (MBC7 cartridges).
    Tilt = 8,
    /// Disk side swap (FDS, QD).
    DiskSwap = 9,
    /// CPU debugger.
    Debugger = 10,
    /// Instruction or call trace to a file.
    Trace = 11,
    /// Memory viewer and RAM search.
    MemoryView = 12,
    /// Movie record and playback.
    Movies = 13,
    /// TAS editor and greenzone.
    Tas = 14,
    /// Run-ahead.
    RunAhead = 15,
    /// HD packs.
    HdPacks = 16,
    /// CRT and NTSC filters.
    CrtNtsc = 17,
    /// Shaders.
    Shaders = 18,
    /// The offline RetroAchievements engine.
    RetroAchievements = 19,
    /// NSF player controls (track select).
    NsfPlayer = 20,
    /// Vs. System coins and DIP switches.
    VsSystem = 21,
}

/// A set of capabilities, one bit per `Capability` discriminant.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Capabilities(pub u32);

impl Capabilities {
    /// No capability.
    pub const NONE: Capabilities = Capabilities(0);

    /// The set plus `cap`.
    pub const fn with(self, cap: Capability) -> Capabilities {
        Capabilities(self.0 | (1 << cap as u32))
    }

    /// Whether `cap` is in the set.
    pub const fn has(self, cap: Capability) -> bool {
        (self.0 & (1 << cap as u32)) != 0
    }
}

/// What the status bar, per-game settings and cheat files key on.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct CartIdentity {
    /// The header title (GB `$0134-$0143` up to NUL, GBA `$0A0-$0AB`, SNES `$FFC0-$FFD4`). For
    /// formats with no title (NES cartridges, FDS), the file stem.
    pub title: String,
    /// CRC-32 (IEEE), computed as each source computes it today (§2.6).
    pub crc32: u32,
}

/// Why a save state was not loaded. `Display` prints the full status line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StateRefusal {
    /// Neither a TendyEmu container nor a state from one of the earlier apps.
    NotAState,
    /// A container written by a newer TendyEmu.
    NewerContainer(u16),
    /// A state of another system.
    WrongSystem {
        /// The system the state belongs to.
        state: SystemKind,
        /// The system that is running.
        running: SystemKind,
    },
    /// The container ends before its blob does.
    Truncated,
    /// The core refused the blob; the text is the core's own message, unchanged.
    Core(String),
}

impl fmt::Display for StateRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StateRefusal::NotAState => write!(f, "refused: not a save state"),
            StateRefusal::NewerContainer(v) => {
                write!(
                    f,
                    "refused: save state format {v} is newer than this TendyEmu"
                )
            }
            StateRefusal::WrongSystem { state, running } => write!(
                f,
                "refused: this is a {} save state; a {} game is running",
                state.display_name(),
                running.display_name()
            ),
            StateRefusal::Truncated => write!(f, "refused: the save state is cut short"),
            StateRefusal::Core(why) => write!(f, "refused: {why}"),
        }
    }
}

impl std::error::Error for StateRefusal {}

/// One running game. The front end holds a `Box<dyn Machine>`; it knows nothing else about a core
/// except through `as_any` (§2.3). Method sources are in §2.2.
pub trait Machine: Any {
    /// The system this core emulates.
    fn kind(&self) -> SystemKind;
    /// The console name the status bar shows, e.g. "Game Boy Color" or "Super Nintendo (NTSC)".
    fn console_name(&self) -> &'static str;
    /// Title and CRC-32 of the loaded image.
    fn cart_identity(&self) -> CartIdentity;
    /// The cartridge board and storage line the status bar shows, e.g. "MBC5 + RAM + battery".
    fn cart_storage(&self) -> String;
    /// What the menu may offer for this game. Asked each time the menu opens, because it can
    /// change, e.g. when an SGB sound cartridge is loaded.
    fn capabilities(&self) -> Capabilities;

    /// Runs one video frame. Returns the core's cycles run, in its own clock units (status/diagnostics only).
    fn run_frame(&mut self) -> u32;
    /// Frames run since power-on.
    fn frames(&self) -> u64;
    /// Frames per second the pacer runs this core at.
    fn frame_rate(&self) -> f64;
    /// Presses the reset button.
    fn reset(&mut self);
    /// The last finished picture, converted to the boundary format.
    fn frame(&mut self) -> Frame<'_>;
    /// A 64-bit hash of the core's native picture buffer: the inertness pin (§5.6). It is never
    /// computed from the RGBA buffer.
    fn frame_checksum(&self) -> u64;

    /// Sets the rate `drain_audio` produces at, in Hz.
    fn set_sample_rate(&mut self, hz: u32);
    /// The rate `drain_audio` produces at, in Hz.
    fn sample_rate(&self) -> u32;
    /// Moves queued audio into `out`: interleaved stereo `f32` (left, right), at `sample_rate()`.
    /// Returns the number of stereo frames written (at most `out.len() / 2`).
    fn drain_audio(&mut self, out: &mut [f32]) -> usize;
    /// The names of the channels `set_channel_mask` controls, bit 0 first.
    fn channel_names(&self) -> &'static [&'static str];
    /// Mutes channels: bit n set means channel n plays.
    fn set_channel_mask(&mut self, mask: u8);

    /// The logical pad for player 1 (§2.5). NES ports 2-4 and NES extra devices go through the NES module (§6.1).
    fn set_buttons(&mut self, buttons: Buttons);

    /// Battery-backed memory (plus RTC where the cartridge has one), or `None` when there is no battery.
    fn battery_body(&self) -> Option<Vec<u8>>;
    /// Loads a battery body. `now_ms` is wall-clock milliseconds since the UNIX epoch, so the RTC can catch up.
    fn load_battery_body(&mut self, bytes: &[u8], now_ms: u64) -> Result<bool, String>;

    /// The core's own state blob, byte-identical to the source's `save_state` (the container is added by `state::wrap`).
    fn save_state(&mut self) -> Vec<u8>;
    /// Loads a blob that `state::unwrap` returned.
    fn load_state(&mut self, blob: &[u8]) -> Result<(), StateRefusal>;

    /// The concrete core, so a system's own UI module can reach features outside this trait (§2.3).
    fn as_any(&self) -> &dyn Any;
    /// The concrete core, mutably.
    fn as_any_mut(&mut self) -> &mut dyn Any;

    /// Why `blob` would be refused before it is loaded, if the core can tell (gb only today).
    fn state_refusal(&self, _blob: &[u8]) -> Option<String> {
        None
    }
    /// Advances a cartridge real-time clock to `now_ms` (GB MBC3/HuC3 only).
    fn tick_rtc(&mut self, _now_ms: u64) {}
    /// Shows or hides the Super Game Boy border. When shown, `frame()` is 256x224.
    fn set_sgb_border(&mut self, _on: bool) {}
    /// Tilt, -128..=127 on each axis (MBC7 only).
    fn set_tilt(&mut self, _x: i8, _y: i8) {}
    /// The inserted disk and side as (side index, side count), for disk systems.
    fn disk_side(&self) -> Option<(usize, usize)> {
        None
    }
    /// Ejects and inserts the next side. Returns the new (side index, side count).
    fn next_disk_side(&mut self) -> Option<(usize, usize)> {
        None
    }
}

/// FNV-1a 64 over `bytes`: the hash the inertness pins use. It is the same function as gb
/// `src/state.rs`, nes `src/state.rs:39` and snes `src/state.rs:28`.
pub fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}
