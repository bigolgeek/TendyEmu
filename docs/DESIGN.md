# TendyEmu engineering design (DESIGN.md)

Status: draft for review (#3). Implements SPEC.md v0.1 (approved, #1). It freezes SPEC §5 for M2–M6
(#4–#36). A change to a declaration here after #4 merges is a DESIGN amendment, made through an issue.

How to read this document:

- Sources are the three earlier projects at the commits the inertness pins come from:

  | Name used here | Project | Commit |
  |---|---|---|
  | gb | gb-rust (DMG/CGB/SGB and GBA) | `449154a` |
  | nes | nes-rust | `efc1e3c` |
  | snes | snes-rust | `bf3c60b` |

- `gb src/system.rs:206` means file and line in that source at that commit. "new" means no source has it.
- Format facts cite a public reference (list in §3.7) or the source file that already does the check.
  Anything not confirmed is marked UNVERIFIED, with the check that settles it.
- "Inert" means SPEC principle 3: after a move, a core produces the same frame checksums, audio hashes
  and state bytes as its source commit.

---

## 1. Workspace layout

### 1.1 Folders

```
Cargo.toml                 workspace root (virtual manifest)
Cargo.lock
crates/
  tendy-core/              the contract: Machine, Frame, Buttons, Capability, state container, hashes
  tendy-detect/            SPEC §4: signatures, extension fallback, refusals, zip/7z
  tendy-spc/               SPC700 + S-DSP, shared by tendy-snes and SGB sound in tendy-gb
  tendy-gb/                DMG, CGB, SGB
  tendy-gba/               Game Boy Advance
  tendy-nes/               NES: cartridges, FDS/QD, UNIF, NSF, Vs. System, NES tools
  tendy-snes/              Super NES
app/
  tendyemu/                the one binary, `tendyemu`, plus a library target for its tests
baselines/                 inertness pins and test-count baselines (#5)
tools/pin/                 the pin probes run against each source commit (#5)
scripts/                   test-ROM manifest and fetch script (no ROMs, no local paths)
docs/                      DESIGN.md
```

SPEC §5 names the crates but not their folder. They live under `crates/` so the root stays short.

### 1.2 Crate graph (no cycles)

| Crate | Depends on (path) | Depends on (external) |
|---|---|---|
| tendy-core | — | — |
| tendy-spc | — | — |
| tendy-gb | tendy-core, tendy-spc | libm, if a moved file uses it |
| tendy-gba | tendy-core, tendy-gb | — |
| tendy-nes | tendy-core | libm (VRC7 OPLL), png (HD-pack decode), zip (BizHawk movie import) |
| tendy-snes | tendy-core, tendy-spc | — |
| tendy-detect | tendy-core, tendy-snes | zip, sevenz-rust2 |
| app/tendyemu | all of the above | macroquad, cpal, rfd, png if screenshots keep using it |

```
                       app/tendyemu
      ┌──────────┬──────────┼──────────┬───────────┐
 tendy-detect  tendy-gba  tendy-nes  tendy-snes  tendy-gb
      │            │          │         │   │       │  │
      └──► tendy-snes   tendy-gb ◄──────┘   │       │  │
                                            ▼       ▼  │
                                         tendy-spc ◄───┘
             every crate ──► tendy-core (except tendy-spc)
```

Why each edge exists:

- **tendy-gb → tendy-spc.** SGB sound runs the SGB cartridge's sound program on the SPC700 (gb `src/sgb/sound.rs`).
  The edge exists only after #6. Until then, tendy-gb carries its own copy (see §4).
- **tendy-snes → tendy-spc.** The SNES APU. Same timing as above: the edge exists after #6.
- **tendy-gba → tendy-gb.** gb `src/gba/` uses gb modules: `link` 11 times, `state` 4, `serial` 4, `joypad` 2, and
  `checksum`, `palette`, `cheats`, `cgb`, `debugger` once each (counted with `git grep 'crate::'` at `449154a`).
  - Copying those modules would duplicate code. Moving them to tendy-core would put GB internals into the shared contract.
  - So tendy-gba depends on tendy-gb. tendy-gb must never depend on tendy-gba.
- **tendy-detect → tendy-snes.** #12 reuses the SNES core's header scoring unchanged (snes
  `src/cart/header.rs:165` `score`). Depending on the core is cheaper than a copy that drifts.

### 1.3 Root `Cargo.toml`

```toml
[workspace]
resolver = "3"
members = ["crates/*", "app/tendyemu"]

[workspace.package]
version = "0.1.0"
edition = "2024"
license = "MIT OR Apache-2.0"

[workspace.dependencies]
tendy-core = { path = "crates/tendy-core" }
tendy-detect = { path = "crates/tendy-detect" }
tendy-spc = { path = "crates/tendy-spc" }
tendy-gb = { path = "crates/tendy-gb" }
tendy-gba = { path = "crates/tendy-gba" }
tendy-nes = { path = "crates/tendy-nes" }
tendy-snes = { path = "crates/tendy-snes" }
cpal = "0.18"
libm = "0.2"
macroquad = "0.4"
png = "0.17"
rfd = "0.17.2"
sevenz-rust2 = { version = "0.25", default-features = false, features = ["compress", "ppmd"] }
zip = { version = "2", default-features = false, features = ["deflate"] }

[profile.release]
opt-level = 3
lto = "thin"
codegen-units = 1
```

| Item | Choice | Reason |
|---|---|---|
| Versions | as above | The three source `Cargo.toml` files agree on every shared crate, so nothing needed choosing. `png 0.17` is in nes only. |
| Edition | 2024 | All three sources use it. A virtual manifest has no edition of its own, so it must set `resolver = "3"`. Needs Rust 1.85 or newer. |
| Licence | MIT OR Apache-2.0 | SPEC principle and all three sources. `LICENSE-MIT` and `LICENSE-APACHE` are already at the root. Every crate sets `license.workspace = true`. |
| Release profile | snes's | snes is the only source with one. gb and nes build with the default profile. |
| `[features]` | nes `ppu-profile = []` moves to tendy-nes | Feature flags are per crate. |
| New dependencies | none | The sources parse `config.toml` by hand, with no toml/serde crate. TendyEmu keeps that parser (§6.2). |
| Lockfile | one `Cargo.lock`, created by #4 | The only file every lane may touch. On a merge conflict, regenerate it with `cargo metadata`; never merge it by hand. |

---

## 2. Exact declarations

### 2.1 `tendy-core` (all of it; written for real by #4, except `state::wrap`/`unwrap`, which #22 implements, and `pin::parse`, which #5 implements)

In the crate, `state` and `pin` are the files `src/state.rs` and `src/pin.rs`. They are shown inline here so the whole
contract reads in one place.

```rust
//! tendy-core: the contract between the front end and every core. No emulation code lives here.

use std::any::Any;
use std::fmt;

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
    pub const ALL: [SystemKind; 4] = [SystemKind::Gb, SystemKind::Gba, SystemKind::Nes, SystemKind::Snes];

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
        Frame { width, height, rgba }
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
                write!(f, "refused: save state format {v} is newer than this TendyEmu")
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

/// The save-state container (§2.7).
pub mod state {
    use super::{StateRefusal, SystemKind};

    /// First four bytes of every container.
    pub const MAGIC: [u8; 4] = *b"TNDY";
    /// The container layout version this build writes.
    pub const VERSION: u16 = 1;
    /// Bytes before the blob: magic 4 + version 2 + tag 4 + blob length 4.
    pub const HEADER_LEN: usize = 14;

    /// Wraps a core's blob for `system`.
    pub fn wrap(system: SystemKind, blob: &[u8]) -> Vec<u8> {
        let _ = (system, blob);
        Vec::new()
    }

    /// Returns the blob inside `bytes` if it belongs to `running`. A raw state from an earlier
    /// app (it starts with `running.legacy_state_magic()`) is returned whole.
    pub fn unwrap(bytes: &[u8], running: SystemKind) -> Result<&[u8], StateRefusal> {
        let _ = (bytes, running);
        Err(StateRefusal::NotAState)
    }
}

/// Parsing for the pin files under `baselines/` (§5.6).
pub mod pin {
    /// One `key = value` line from a pin file.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct Pin {
        /// The run this pin belongs to, e.g. `gb/dmg-acid2`.
        pub run: String,
        /// What was measured, e.g. `frame@600`, `audio`, `state`, `state_len`.
        pub key: String,
        /// The value, lower-case hex or decimal, exactly as written.
        pub value: String,
    }

    /// Parses a pin file. Lines are `[run]` headers, `key = value` pairs, blanks, or `#` comments.
    pub fn parse(text: &str) -> Result<Vec<Pin>, String> {
        let _ = text;
        Ok(Vec::new())
    }
}
```

The two stub bodies (`state::wrap`/`unwrap`, `pin::parse`) are zero-value stubs written by #4. #22 and #5
replace them. They must compile with no `todo!`, as #4 requires.

### 2.2 Where every `Machine` method comes from

"gb" means `impl System for Machine` (gb `src/system.rs:446`). "GBA" means `impl System for Gba`
(gb `src/gba/mod.rs:602`); its methods have the same names as gb's. The nes column describes the new
`NesMachine` wrapper over `pub struct Nes` (nes `src/nes.rs:261`), because nes has no trait.

| Machine method | gb (and GBA) | snes | nes |
|---|---|---|---|
| kind | new (gb `is_advance` `system.rs:263` picks Gb/Gba) | new | new |
| console_name | `system.rs:259` console_name | `system.rs:65` console_name | new: from `Nes::region` `nes.rs:518` |
| cart_identity | `system.rs:290` cart_identity | `system.rs:66` cart_identity | new: stem + CRC-32 of PRG‖CHR (gamedb key, nes `src/checksum.rs` crc32) |
| cart_storage | `system.rs:270` cartridge_storage | `system.rs:68` cart_storage | new: mapper + `Nes::battery` `nes.rs:267` |
| capabilities | new | new | new |
| run_frame | `system.rs:210` run_frame | `system.rs:58` run_frame | `Nes::run_frame` `nes.rs:579`; the return value is new (cpu cycle delta) |
| frames | `system.rs:250` frames | `system.rs:62` frames | new (wrapper counter) |
| frame_rate | new (value §2.4) | new: `FPS` (snes `src/lib.rs`) | new: per region (nes `src/timing.rs`) |
| reset | `system.rs:311` reset | `system.rs:71` reset | `Nes::reset` `nes.rs:533` |
| frame | `system.rs:213` frame + conversion §2.4 | `system.rs:59` frame + `picture::bgr555_to_rgba` | `ppu.framebuffer`/`ppu.emphasis` (nes `src/ppu/mod.rs:143/146`) + `palette::to_rgba_pal` |
| frame_checksum | `system.rs:256` frame_checksum (`machine.rs:352-369`) | `system.rs:63` frame_checksum | new: `fnv1a64(framebuffer ‖ emphasis)` |
| set_sample_rate | `system.rs:316` set_sample_rate | `system.rs:72` set_sample_rate | `Apu::set_sample_rate(f64)` nes `src/apu/mod.rs:256` |
| sample_rate | new (the wrapper stores the rate) | `system.rs:76` sample_rate | `apu.sample_rate` field (`apu/mod.rs:93`) |
| drain_audio | `system.rs:236` drain_audio | `system.rs:60` drain_audio | new: interleaves `apu.samples`/`apu.samples_right` as nes `main.rs` `interleave_stereo` does |
| channel_names | new (the four rows gb's `set_channel_mask` doc names) | new | new: `Channel::ALL` (nes `src/apu/gain.rs:36`) |
| set_channel_mask | `system.rs:319` set_channel_mask | `system.rs:73` set_channel_mask | new: bit n → `ChannelGain` 0.0 or its setting (nes `apu/gain.rs:81`) |
| set_buttons | `system.rs:239` set_buttons + `system.rs:243` set_shoulders | `system.rs:61` set_buttons | `pads[0].set_buttons(u8)` nes `src/controller.rs:36` |
| battery_body | `system.rs:355` battery_body | `system.rs:74` battery_body | `save::battery_snapshot` nes `src/save.rs:38` |
| load_battery_body | `system.rs:359` load_battery_body | `system.rs:75` (ignores `now_ms`) | the body half of `save::load_save` nes `src/save.rs:53` |
| save_state | `system.rs:295` save_state | `system.rs:69` save_state | `Nes::save_state` `nes.rs:591` |
| load_state | `system.rs:298` load_state (`StateError` → `Core(text)`) | `system.rs:70` load_state | `Nes::load_state` `nes.rs:610` |
| as_any, as_any_mut | new | new | new |
| state_refusal | `system.rs:306` state_refusal | default | default |
| tick_rtc | `system.rs:322` tick_rtc | default | default |
| set_sgb_border | new over `system.rs:220` sgb_mask, `:225` sgb_border, `SgbScreen::compose` `system.rs:120` | default | default |
| set_tilt | `system.rs:246` set_tilt | default | default |
| disk_side | default | default | `cart().disk_side()` nes `src/cart.rs:643` |
| next_disk_side | default | default | `cart_mut().next_disk_side()` nes `src/cart.rs:638` |

Source methods that are not on `Machine`, and where they go:

| Source method | Goes to | Reached by |
|---|---|---|
| gb `is_advance` 263, `booting` 253, `is_cgb` 274, `is_cgb_console` 279, `cgb_speed` 282, `compat_palette` 287 | stay on gb's `System` trait in tendy-gb | app `gb` module via `as_any_mut` (§2.3) |
| gb `take_serial` 325, `take_link_notice` 328, `set_link` 332, `attach_link` 343, `link_status` 349, `take_link_message` 352 | gb `System` trait | app `gb` module |
| gb `apply_cheats` 363 | gb `System` trait; cheats are a per-core UI (SPEC §6) | app `gb` module |
| gb `step_instruction` 368, `pc` 376, `sp` 379, `cpu_state` 384, `cpu_regs` 387, `advance_regs` 392, `peek` 400, `vram` 403, `oam` 406, `view_regs` 409, `cycles` 413, `vram_dma_blocks` 417, `instructions` 431, `trace_swis` 438 | gb `System` trait (the debugger model) | app `gb` module |
| gb `effect_state` 425, `swi_trace` 441 (they name `crate::gba` types) | inherent methods on tendy-gba's `Gba` | app `gb` module, GBA branch |
| nes everything else (movies, TAS, run-ahead, HD packs, Zapper/Four Score/Power Pad/paddle/keyboard, Vs., NSF song change, cheats, debugger, trace, CDL, memory view, overclock, layers) | inherent on `NesMachine.nes` (the source API, unchanged) | app `nes` module via `as_any_mut` |

### 2.3 Capabilities and how the menu asks

```rust
// app/tendyemu/src/caps.rs (#16)

/// A menu row. `requires` is `None` for rows every core has (open, states, rewind, screenshot...).
pub struct RowGate {
    /// The capability the row needs, if any.
    pub requires: Option<tendy_core::Capability>,
}

/// Whether a row is shown for the running core.
pub fn visible(gate: &RowGate, caps: tendy_core::Capabilities) -> bool {
    gate.requires.is_none_or(|c| caps.has(c))
}
```

The menu calls `machine.capabilities()` each time the drawer opens, and filters rows with `visible`.
A system's own UI module reaches its concrete core like this:

```rust
if let Some(nes) = machine.as_any_mut().downcast_mut::<tendy_nes::NesMachine>() { /* NES rows */ }
```

Capabilities each core reports (the #16 tests assert exactly these):

| Core | Always | Only when |
|---|---|---|
| tendy-gb | ChannelMask, Cheats, BootRom, Palettes, LcdFilter, Link, Debugger, Trace | SgbBorder: running as SGB · SgbSound: SGB with an SGB sound cartridge loaded · Tilt: MBC7 cartridge |
| tendy-gba | ChannelMask, Cheats, Link, Debugger, Trace | — |
| tendy-nes | ChannelMask, Cheats, Palettes, Movies, Tas, RunAhead, HdPacks, CrtNtsc, Shaders, Debugger, Trace, MemoryView, RetroAchievements | DiskSwap: FDS/QD · NsfPlayer: NSF · VsSystem: Vs. header |
| tendy-snes | ChannelMask | — |

Two of these are UNVERIFIED:

- Whether gb applies `LcdFilter` to GBA too. Settle by grepping gb `src/main.rs` for the filter's condition.
- Whether the nes app offers each NES row for NSF and FDS too. Settle with the #26 checklist.

Rewind, states, screenshots and recording work on every core, so they need no capability (SPEC §6).

### 2.4 Frame: each core's conversion to RGBA8888

| Core | Native buffer (stays, and is what `frame_checksum` hashes) | Conversion in `frame()` | Native size |
|---|---|---|---|
| GB DMG | 2-bit shades (gb `Frame.pixels`) | the selected built-in palette, shade → RGBA (gb `src/palette.rs`) | 160x144 |
| GB CGB | 15-bit colours (gb `Frame.colours`) | `palette::rgb555` (gb `src/palette.rs:61`): `expand(v) = (v << 3) \| (v >> 2)`, red in bits 0-4 | 160x144 |
| SGB | shades + SGB palettes (+ border) | `SgbScreen::compose` (gb `src/system.rs:120`) → 15-bit → `rgb555` | 160x144, or 256x224 with border |
| GBA | 15-bit colours (gb `src/gba/mod.rs` frame) | `palette::rgb555` | 240x160 |
| NES | palette indices 0..=63 + emphasis bits | `palette::to_rgba_pal(frame, emph, table, out)` (nes `src/ppu/palette.rs:84`); the table comes from NES settings (built-in, `.pal`, Vs. RGB PPU) | 256x240 |
| SNES | 15-bit words, red in bits 0-4 | `picture::bgr555_to_rgba` (snes `src/picture.rs:17`) | 256x224 |

Notes:

- Each wrapper owns its own `rgba: Vec<u8>` and fills it inside `frame()`. That is why `frame` takes `&mut self`.
- The NES NTSC filter and HD packs need the native indices. The app `nes` module reads them through `as_any_mut`, as nes `main.rs` does today.
- GB and SNES "RGB555" and "BGR555" are the same bit layout: red in the low five bits (gb `palette.rs:61`,
  snes `picture.rs`, fullsnes "CGRAM"). Each core keeps its own function anyway, because the move must be inert.

`frame_rate` values:

| Core | Rate | Source |
|---|---|---|
| GB | 4194304 / 70224 Hz | Pan Docs: 70224 dots per frame |
| GBA | 16777216 / 280896 Hz | GBATEK: 228 lines × 1232 cycles |
| NES | from `Nes::region` | nes `src/timing.rs` |
| SNES | `FPS` 60.098_475_52 | snes `src/lib.rs:28` |

UNVERIFIED: that gb `src/timing.rs` and nes `src/timing.rs` pace with exactly these values. Settle by grepping
each pacer's rate constant. If one differs, `frame_rate` returns the source's constant.

### 2.5 Logical buttons → each core's pad

| Logical | GB / GBA `joypad::Buttons(u8)` | GBA shoulders `Shoulders(u8)` | NES pad `u8` (nes `controller.rs:27`) | SNES `joypad::Buttons(u16)` |
|---|---|---|---|---|
| UP | bit 2 | — | 0x10 | 1<<11 |
| DOWN | bit 3 | — | 0x20 | 1<<10 |
| LEFT | bit 1 | — | 0x40 | 1<<9 |
| RIGHT | bit 0 | — | 0x80 | 1<<8 |
| A | bit 4 | — | 0x01 | 1<<7 |
| B | bit 5 | — | 0x02 | 1<<15 |
| X | — | — | — | 1<<6 |
| Y | — | — | — | 1<<14 |
| L | — | bit 0 | — | 1<<5 |
| R | — | bit 1 | — | 1<<4 |
| START | bit 7 | — | 0x08 | 1<<12 |
| SELECT | bit 6 | — | 0x04 | 1<<13 |

Mapping rules:

- The pad layouts come from the source files: gb `src/joypad.rs`, nes `src/controller.rs:27`, snes `src/joypad.rs:19`.
- A button a system lacks is ignored.
- Each wrapper converts in `set_buttons` with a `const` table; nothing else changes.
- Bindings are per system (§6.3).

### 2.6 `CartIdentity` per core

| Core | title | crc32 | Source |
|---|---|---|---|
| GB | `$0134-$0143` up to NUL, printable only | whole ROM | gb `src/system.rs:526` cart_identity |
| GBA | `$0A0-$0AB` | whole ROM | gb `src/gba/mod.rs:676` cart_identity |
| NES | file stem (NSF: song title, `$00E`) | PRG‖CHR; FDS/NSF: whole file | new, using nes `src/checksum.rs:87` crc32 |
| SNES | `$FFC0-$FFD4` (as parsed) | whole ROM | snes `src/system.rs:108` cart_identity |

NES keeps its other hashes inside the core: FNV-1a of the file for states (nes `src/state.rs:39` fnv1a64) and MD5 for cheat files.
UNVERIFIED: that the NES game database keys on CRC-32 of PRG‖CHR (from the inventory, not re-read). Settle by reading
the key function in nes `src/gamedb.rs`. If it differs, `cart_identity` uses the whole-file CRC-32.

### 2.7 Save-state container

| Offset | Size | Field |
|---|---|---|
| 0 | 4 | `TNDY` |
| 4 | 2 | container version, u16 little-endian (= 1) |
| 6 | 4 | system tag (`SystemKind::tag`) |
| 10 | 4 | blob length n, u32 little-endian |
| 14 | n | the core's blob: the exact bytes the source's `save_state` writes (gb `GBST` v10, GBA `GBAG`, nes `NESS` v12, snes `SNST` v2) |

`unwrap(bytes, running)` checks, in order:

1. Starts with `running.legacy_state_magic()` → returns `bytes` whole (states copied from the earlier apps).
2. Starts with another system's legacy magic → `WrongSystem`.
3. Not `TNDY` → `NotAState`.
4. Version > `VERSION` → `NewerContainer`.
5. Unknown tag → `NotAState`.
6. Tag ≠ running → `WrongSystem`.
7. Length past the end → `Truncated`.
8. Otherwise returns the blob.

The core then checks its own magic, version and checksum, as today. Rewind and run-ahead use raw blobs
in memory; only files carry the container.

### 2.8 Detection API (`tendy-detect`, `src/lib.rs`, written in full by #4)

```rust
//! SPEC §4: decide which core runs a file, or refuse it.

pub mod archive;
pub mod extension;
pub mod signature;

pub use archive::{open, ArchiveKind, Entry};

use std::fmt;
use tendy_core::SystemKind;

/// How the system was decided.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Basis {
    /// A header signature matched (§3.2).
    Signature,
    /// No signature matched; the extension decided (§3.3).
    Extension,
}

/// The file layout, as far as detection can tell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    /// iNES 1.0 header.
    Ines,
    /// NES 2.0 header.
    Nes20,
    /// UNIF chunks.
    Unif,
    /// FDS image with the 16-byte fwNES header.
    FdsHeadered,
    /// FDS image without a header (starts at disk block 1).
    FdsRaw,
    /// QD disk image (raw disk signature plus the `.qd` extension).
    Qd,
    /// NSF music file.
    Nsf,
    /// Game Boy Advance cartridge.
    Gba,
    /// Game Boy, Color or Super Game Boy cartridge.
    Gb,
    /// Super NES cartridge.
    Snes,
}

/// Game Boy header flags. Information only in v0.1: the model is chosen as today (§3.4).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GbFlags {
    /// `$0143` bit 7: the cartridge uses Color features.
    pub cgb: bool,
    /// `$0146 == $03` and `$014B == $33`: the cartridge supports Super Game Boy functions.
    pub sgb: bool,
}

/// What `detect` decided.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Detected {
    /// The core to build.
    pub system: SystemKind,
    /// The layout inside that system.
    pub format: Format,
    /// Signature or extension.
    pub basis: Basis,
    /// The one-time conflict notice, when the extension named another system (§3.3).
    pub notice: Option<String>,
    /// Game Boy flags; `None` for other systems.
    pub gb: Option<GbFlags>,
    /// SNES: a 512-byte copier header is present (size mod 1024 = 512). The core strips it, as today.
    pub copier_header: bool,
}

/// A file the loader will not open. `Display` prints the whole line, starting `refused: `.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Refusal {
    /// The words after `refused: `.
    pub reason: String,
}

impl Refusal {
    /// `refused: not a recognised ROM (<file>)` (SPEC §4 step 5).
    pub fn not_a_rom(file: &str) -> Refusal {
        Refusal { reason: format!("not a recognised ROM ({file})") }
    }

    /// `refused: no ROM inside <archive>` (SPEC §4 step 1).
    pub fn no_rom_inside(archive: &str) -> Refusal {
        Refusal { reason: format!("no ROM inside {archive}") }
    }
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "refused: {}", self.reason)
    }
}

impl std::error::Error for Refusal {}

/// Decides which core runs `bytes`. `name` is a file or archive-entry name; only its extension is
/// read. Signatures first (§3.2), then extension fallback, conflict notice and refusal (§3.3).
pub fn detect(name: &str, bytes: &[u8]) -> Result<Detected, Refusal> {
    let by_signature = signature::identify(bytes);
    extension::resolve(name, by_signature)
}
```

```rust
// crates/tendy-detect/src/signature.rs (#12)

/// A signature match.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Match {
    /// The core.
    pub system: tendy_core::SystemKind,
    /// The layout (`FdsRaw` here even for a `.qd` file; `extension::resolve` turns it into `Qd`).
    pub format: crate::Format,
    /// Game Boy flags, for `Format::Gb`.
    pub gb: Option<crate::GbFlags>,
    /// SNES copier header present.
    pub copier_header: bool,
}

/// The lowest SNES header score (of 13) that counts as a signature (§3.2 row 8, §9 Q2).
pub const SNES_SIGNATURE_MIN_SCORE: i32 = 8;

/// Runs the §3.2 table in order and returns the first row that matches.
pub fn identify(bytes: &[u8]) -> Option<Match> {
    let _ = bytes;
    None
}
```

```rust
// crates/tendy-detect/src/extension.rs (#13)

/// The system an extension names (§3.3 table). Case-insensitive, without the dot.
pub fn system_of(ext: &str) -> Option<tendy_core::SystemKind> {
    let _ = ext;
    None
}

/// SPEC §4 steps 3-5: the extension fallback, the conflict notice and the refusal.
pub fn resolve(
    name: &str,
    by_signature: Option<crate::signature::Match>,
) -> Result<crate::Detected, crate::Refusal> {
    let _ = by_signature;
    Err(crate::Refusal::not_a_rom(name))
}
```

### 2.9 Archive extraction API (`crates/tendy-detect/src/archive.rs`, #14)

```rust
/// The archive formats SPEC §3 lists.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArchiveKind {
    /// `.zip`, read with `zip` (deflate only).
    Zip,
    /// `.7z`, read with `sevenz-rust2`.
    SevenZ,
}

impl ArchiveKind {
    /// The kind a file name's extension names (case-insensitive), as all three sources decide today.
    pub fn from_name(name: &str) -> Option<ArchiveKind> {
        let _ = name;
        None
    }
}

/// The largest entry the loader reads, in bytes. `sevenz-rust2`'s memory limit covers only its own
/// buffers and dictionaries, not the size of what it extracts (docs.rs, sevenz-rust2 0.25.0,
/// `ArchiveReader::new_with_memory_limit`), so this cap is enforced by reading through `Read::take`.
pub const MAX_ENTRY_BYTES: u64 = 64 * 1024 * 1024;

/// One file, taken from disk or out of an archive.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// The file name, or the entry's path inside the archive.
    pub name: String,
    /// Its bytes.
    pub bytes: Vec<u8>,
}

/// Picks the ROM inside an archive (SPEC §4 step 1).
/// - Pass 1: the first file entry, in stored order, whose extension `extension::system_of` knows.
/// - Pass 2: the first file entry whose bytes match a signature.
/// - Otherwise `Refusal::no_rom_inside`.
///
/// Folders are skipped, nested paths count, and entries over `MAX_ENTRY_BYTES` are skipped.
pub fn extract(kind: ArchiveKind, archive_name: &str, bytes: &[u8]) -> Result<(Entry, crate::Detected), crate::Refusal> {
    let _ = (kind, bytes);
    Err(crate::Refusal::no_rom_inside(archive_name))
}

/// The loader's one entry point: archives go to `extract`, anything else straight to `crate::detect`.
pub fn open(name: &str, bytes: Vec<u8>) -> Result<(Entry, crate::Detected), crate::Refusal> {
    let detected = crate::detect(name, &bytes)?;
    Ok((Entry { name: name.to_owned(), bytes }, detected))
}
```

What the archive code reuses, and what it changes:

- It reuses the read calls of gb `src/launch.rs` `load_zip` (:269) and `load_7z` (:304), which use the same crate versions.
  gb's is the only source loader that already picks between two consoles.
- A corrupt archive is refused as `refused: cannot read <archive>: <error>`, the wording gb and snes use today.
- The per-app wordings "no .gb entry and no .gba entry in …", "no .smc or .sfc entry in …" and
  "no .nes, .fds, .qd, .unf or .nsf entry in …" become SPEC's single `no ROM inside <archive>`.

### 2.10 Core constructors (one per core crate; the app maps `Detected` to these)

```rust
// tendy-gb (#7)
/// The Game Boy model to run, as gb's console setting does today (gb `src/config.rs`; `--cgb`, `--sgb`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum GbModel { #[default] Auto, Dmg, Cgb, Sgb }
/// What gb's launch path passes to the machine today.
#[derive(Clone, Debug, Default)]
pub struct GbOptions {
    /// Model; `Auto` picks DMG or CGB from `$0143` bit 7, never SGB (gb `src/main.rs:575`).
    pub model: GbModel,
    /// A user boot ROM (gb `launch.rs:354` load_boot_rom).
    pub boot_rom: Option<Vec<u8>>,
    /// The SGB cartridge for SGB sound: 262144 bytes, `Super GAMEBOY` at `$7FC0` (gb `src/sgb/sound.rs:328`).
    pub sgb_sound_rom: Option<Vec<u8>>,
}
/// The Machine wrapper over gb's machine (`src/machine.rs`) plus its RGBA buffer.
pub struct GbMachine { /* core: machine::Machine, rgba: Vec<u8>, palette, border flag */ }
impl GbMachine {
    /// Builds the machine; the error text is gb's own refusal text.
    pub fn new(rom: Vec<u8>, options: GbOptions) -> Result<GbMachine, String> { let _ = (rom, options); Err(String::new()) }
}

// tendy-gba (#8)
/// The Machine wrapper over gb's `Gba` (`src/gba/mod.rs`).
pub struct GbaMachine { /* core: Gba, rgba: Vec<u8> */ }
impl GbaMachine {
    /// Builds the machine; refusals are gb `src/gba/cart.rs` `Header::parse` texts.
    pub fn new(rom: Vec<u8>, bios: Option<Vec<u8>>) -> Result<GbaMachine, String> { let _ = (rom, bios); Err(String::new()) }
}

// tendy-nes (#9)
/// Which nes constructor runs: `from_rom_data_console` (`nes.rs:376`), `from_fds_console` (`nes.rs:452`) or `from_nsf` (`nes.rs:494`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NesImage { Cartridge, Disk { qd: bool }, Nsf { song: u8 } }
/// The Machine wrapper; `nes` is public so the app's NES module keeps using the source API.
pub struct NesMachine { pub nes: Nes /* , rgba, frames, mask, title, palette table */ }
impl NesMachine {
    /// Builds the machine. `fds_bios` is the user's 8192-byte BIOS (nes `src/mapper/fds.rs:38`).
    pub fn new(image: Vec<u8>, kind: NesImage, fds_bios: Option<Vec<u8>>, console: console::ConsoleConfig) -> Result<NesMachine, String> { let _ = (image, kind, fds_bios, console); Err(String::new()) }
}

// tendy-snes (#10)
/// The Machine wrapper over snes's machine (`src/machine.rs`).
pub struct SnesMachine { /* core: machine::Machine, rgba: Vec<u8> */ }
impl SnesMachine {
    /// Builds the machine; the error is snes `src/cart/refusal.rs` text, starting `refused: `.
    pub fn new(rom: Vec<u8>) -> Result<SnesMachine, String> { let _ = rom; Err(String::new()) }
}
```

Wrapper rules:

- The struct bodies shown as comments are the fields each import issue adds.
- The wrappers sit beside the source files, in a new file `src/machine_impl.rs` in each core crate, and do not edit the
  source files. That keeps the move inert.
- #4 creates the core crates empty: a `lib.rs` with a crate doc only. Each import issue (#7–#10) adds its wrapper with
  exactly these signatures. A signature that cannot compile as written is a DESIGN amendment.
- UNVERIFIED: the name of nes's console-settings type in `src/console.rs`; `ConsoleConfig` above is a placeholder.
  Settle with `git grep -n "pub struct" src/console.rs` in nes at the import (#9). The real name is kept.
- gb's `System` trait stays in tendy-gb under the same name, implemented by gb's `Machine` and by `Gba`.
  `GbMachine::system_mut()` and `GbaMachine::system_mut()` return `&mut dyn tendy_gb::System` for the app's `gb` module.

---

## 3. Detection (SPEC §4)

### 3.1 Order

`tendy_detect::open(name, bytes)` runs these steps:

1. **Archive.** `.zip` or `.7z` by extension → `archive::extract` (§2.9). It applies steps 2–5 to the entry it picks.
2. **Signatures.** The table in §3.2, top to bottom. The first row that matches wins.
3. **Extension fallback.** No signature matched, so the extension decides (§3.3).
4. **Conflict.** A signature matched and the extension names a different system. The signature wins, and the user sees one notice.
5. **Refusal.** No signature and no known extension → `refused: not a recognised ROM (<file>)`.

The order of the table:

- Rows 1–5 are fixed byte strings at offset 0, so they are cheap. "Exact" holds only within a row's own
  system; the residual cross-system risk is accepted: a valid SNES image whose first four bytes are
  `4E 45 53 1A` matches row 1 and is then refused by the NES core with a bad-header message; likewise a GB
  image starting with `4E 45 53 1A` (row 7 never runs); an SNES image crafted to pass the GBA row's branch,
  fixed byte and complement is claimed as GBA (row 6 precedes row 8). All are contrived, the order is frozen
  by SPEC §4, and the failure mode is a refusal with a reason, never a guess (SPEC principle 2).
- GBA comes before GB.
- SNES comes last because it is a score, not a magic number (SPEC §11).

Once a core is chosen, it may still refuse the file with its own text, as today (for example a bad NES header,
an NSF version other than 1, or a SNES coprocessor). Detection never rewrites a core's refusal text.

### 3.2 Signature table (#12)

`b[i]` is the byte at offset `i`. `len` is the file length.

| # | Row | Check | Result | Sources |
|---|---|---|---|---|
| 1 | iNES / NES 2.0 | `len >= 16` and `b[0..4] == 4E 45 53 1A` ("NES" + EOF) | Nes, `Ines`; `Nes20` when `(b[7] & 0x0C) == 0x08` | NESdev "iNES", "NES 2.0" (bit 2 clear, bit 3 set); nes `src/cart.rs:170-175`, `:206` |
| 2 | UNIF | `len >= 32` and `b[0..4] == "UNIF"` | Nes, `Unif` | NESdev "UNIF" (32-byte header); nes `src/unif.rs:29` |
| 3 | FDS, fwNES header | `len >= 16` and `b[0..4] == 46 44 53 1A` ("FDS" + EOF) | Nes, `FdsHeadered` | NESdev "FDS file format" (byte 4 = side count, 65500 bytes per side); nes `src/mapper/fds.rs:26` |
| 4 | FDS / QD, no header | `len >= 15`, `b[0] == 0x01` and `b[1..15] == "*NINTENDO-HVC*"` | Nes, `FdsRaw` (`Qd` when the extension is `.qd`, §3.3) | NESdev "FDS disk format" (block 1, disk info block); nes `src/launch.rs:942`, `:990` |
| 5 | NSF | `len >= 0x80` and `b[0..5] == 4E 45 53 4D 1A` ("NESM" + EOF) | Nes, `Nsf` | NESdev "NSF" (128-byte header); nes `src/nsf.rs:37` |
| 6 | GBA | all of the below | Gba, `Gba` | GBATEK "GBA Cartridge Header"; gb `src/gba/cart.rs` |
| 7 | GB / CGB / SGB | `len >= 0x150` and `b[0x104..0x134]` equals the 48-byte logo | Gb, `Gb`, with `GbFlags` | Pan Docs "The Cartridge Header" (0104-0133); logo bytes as gb `src/cart/mbc1.rs:95` `NINTENDO_LOGO` |
| 8 | SNES | the best header score is at least `SNES_SIGNATURE_MIN_SCORE` (8 of 13) | Snes, `Snes`, `copier_header` | fullsnes "SNES Cartridge ROM Header"; snes `src/cart/header.rs:165` `score`, `src/cart/mod.rs:45` |

The GBA check (row 6) needs all four of these:

- `len >= 0xC0`.
- Bits 27–25 of the little-endian word at 0 are `101`, an ARM branch (GBATEK "ROM Entry Point").
- `b[0xB2] == 0x96`.
- `b[0xBD]` equals the complement check: start `c = 0`; for each `i` in `0xA0..=0xBC`, set `c = c - b[i]`;
  finally `c = (c - 0x19) & 0xFF` (GBATEK "Header Checksum").

The logo at `$004-$09F` is not part of the check. gb's loader refuses on a bad branch, a bad fixed byte or a bad
complement, and only records whether the logo matches (`logo_ok`). Detection matches what the loader accepts.

The SNES check (row 8):

- When `len % 1024 == 512`, a copier header is present: skip the first 512 bytes for scoring and set `copier_header`.
- Score the candidates `$7FC0` (LoROM), `$FFC0` (HiROM) and `$40FFC0` (ExHiROM) with the core's own `score`,
  unchanged (snes `src/cart/header.rs:43` `CANDIDATES`). A later candidate replaces the best only on a strictly
  greater score, so on a tie the earlier one wins (`header.rs:164`).
- The score is out of 13: `$FFD5`'s map nibble 4, its speed bits 1, the checksum pair 4, and the size, title and
  chipset bytes for the other 4 (`header.rs:161`).
- The SNES core itself accepts a best score of 4 or more, unless every byte of the image is the same
  (`header.rs:86`). That is fine when the user said "this is SNES". A signature must also be safe when the file
  says nothing, so it needs 8: a valid map nibble plus a matching checksum pair is already 8.
- A SNES file that scores 4–7 still opens when it has a SNES extension, through the fallback. Only a mis-named weak
  file is refused.
- UNVERIFIED: that 8 separates the test sets cleanly. Settle in #12: run `score` over every GB, GBA and NES image in
  the test-ROM manifest and over random data; all must score below 8. Every SNES manifest ROM must score 8 or more.
  If not, the constant changes by DESIGN amendment.
- A file too short to hold the `$7FC0` header has no candidate inside it, so it never matches row 8. The SNES core
  refuses it with its own text (snes `src/cart/refusal.rs`).

Row 7 flags (`GbFlags`, information only, §3.4):

- `cgb`: `b[0x143] & 0x80 != 0`.
- `sgb`: `b[0x146] == 0x03 && b[0x14B] == 0x33`. Pan Docs: the SGB ignores its functions unless `$0146` is `$03`
  and the old licensee code is `$33`.

### 3.3 Extension fallback, conflict, refusal (#13)

Extensions are matched case-insensitively, on the last dot only.

| Extension | System | Format when no signature matched |
|---|---|---|
| `gb` `gbc` `sgb` | Gb | `Gb` |
| `gba` `agb` | Gba | `Gba` |
| `nes` | Nes | `Ines` (the core refuses a missing header with its own text) |
| `unf` `unif` | Nes | `Unif` |
| `fds` | Nes | `FdsRaw` |
| `qd` | Nes | `Qd` |
| `nsf` | Nes | `Nsf` |
| `sfc` `smc` `swc` `fig` | Snes | `Snes` |
| `zip` `7z` | archive (step 1) | — |

Rules:

| Case | Result |
|---|---|
| Signature matched, extension names the same system | `basis = Signature`, no notice |
| Signature `FdsRaw` and extension `.qd` | `Format::Qd`, no notice |
| Signature matched, extension names another system | open as the signature's system, with SPEC §4's one-line notice naming the extension and the system it opened as |
| Signature matched, extension unknown or missing | open as the signature's system, no notice |
| No signature, known extension | `basis = Extension`; the core decides whether it can run it (logo-less GB homebrew, decoy-header GB boards, raw `.qd`) |
| No signature, unknown extension | `refused: not a recognised ROM (<file>)` |

Patch files (`.ips .bps .ups`) are not ROMs and are not detected here. Applying them belongs to #30.

### 3.4 Game Boy model

Detection reports `GbFlags`, and the model is chosen as gb chooses it today:

- `Auto` picks CGB when `$0143` bit 7 is set, else DMG.
- SGB runs only when the user sets the model to SGB (gb `src/main.rs:575`, `--sgb`, config `console`).
- gb never reads `$0146`.

Making `sgb` pick the model automatically is new behaviour, and goes to the backlog (v0.1 is parity, SPEC §11).

### 3.5 What changes in the loaders

| Source loader | Today | TendyEmu |
|---|---|---|
| gb `src/launch.rs` | extension picks GB or GBA; zip/7z pick the first `.gb`/`.gba` entry | `tendy_detect::open` |
| nes `src/launch.rs` | extension picks cartridge, FDS, QD, UNIF or NSF; zip/7z by extension | `tendy_detect::open`, then `NesImage` from `Format` |
| snes `src/launch.rs` | `.smc`/`.sfc`, zip/7z | `tendy_detect::open` |

Each core's constructor (§2.10) is unchanged behind the wrapper. The only visible change is the wording of the
archive refusals (§2.9) and of the "not a ROM" refusals, which become SPEC's.

### 3.6 SPEC facts corrected

| # | SPEC says | Evidence | Correction |
|---|---|---|---|
| C1 | §4 GB row: `$0146 = 03` → SGB | gb never reads `$0146`; SGB only via setting or `--sgb` (gb `src/main.rs:575`) | The flag is reported; the model is chosen as today (§3.4) |
| C2 | §4 GBA row: logo + `$B2 = 96h` | gb `src/gba/cart.rs` requires branch + `$B2` + complement; logo recorded only | Signature = branch + `$B2` + complement (§3.2 row 6) |
| C3 | §4 GB row implies every GB ROM has the logo | unlicensed boards with decoy headers carry no logo | Logo match is a signature; no logo falls through to the extension (§3.3) |
| C4 | §4 NES 2.0 "byte 7 bits 2–3 = 10" | NESdev: bit 2 clear, bit 3 set; nes `src/cart.rs:206` | `(b[7] & 0x0C) == 0x08` |
| C5 | §4 raw FDS row identifies FDS | a QD side starts with the same block (NESdev "FDS disk format"); nes tells them apart by extension (`src/launch.rs:1169`, `:1219`) | Row 4 picks the NES core; `.qd` vs `.fds` stays extension-based |
| C6 | §3/§4 `.nsf` | nes accepts NSF version 1 only (`src/nsf.rs:86`); NSFe ("NSFE") is unsupported (`src/launch.rs:1809`, `:1817`) | `.nsf` version 1; others refused by the core with its text |
| C7 | §5 and #18 SNES "256/512 x 224/239" | snes outputs 256x224 only: `src/lib.rs:13` "NTSC, no overscan"; hi-res is sampled `x*2` into 256 columns (`src/ppu/layers.rs:85`, `:94`) | SNES native size is 256x224. 512-wide or 239-line output is new behaviour — **owner decision 2026-11-26: backlog** |
| C8 | §3 soft patches "keep working for the cores that support them today" | only nes patches, IPS and BPS; UPS refused (nes `src/patch.rs`); gb and snes have none | Patches work today for NES (IPS, BPS) only; #30 adds the rest |
| C9 | §5 Machine = "union of the three System traits" | nes has no trait; its machine is `struct Nes` (`src/nes.rs:261`) | Machine is the union of gb's and snes's traits plus a new NES wrapper (§2.2) |
| C10 | #8: GBA "shares no hardware" with GB, read as "shares no code" | true of the hardware, but gb `src/gba/` imports gb's `link`, `state`, `serial`, `joypad`, `checksum`, `palette`, `cheats`, `cgb`, `debugger` | tendy-gba depends on tendy-gb (§1.2) |

### 3.7 Public references

| Short name | Document |
|---|---|
| Pan Docs | Pan Docs, "The Cartridge Header" and "Rendering" (gbdev.io) |
| GBATEK | GBATEK, "GBA Cartridge Header" and "LCD" (problemkaputt.de) |
| NESdev | NESdev Wiki: "iNES", "NES 2.0", "UNIF", "FDS file format", "FDS disk format", "NSF" |
| fullsnes | fullsnes, "SNES Cartridge ROM Header" and "CGRAM" (problemkaputt.de) |
| sevenz-rust2 | docs.rs, sevenz-rust2 0.25.0, `ArchiveReader` |

---

## 4. Shared SPC crate (#6)

### 4.1 The two copies

gb's `src/sgb/spc/` and snes's `src/apu/` hold the same five files. Line counts at the source commits, and the five
differences found by diffing them:

| File | snes lines | gb lines |
|---|---|---|
| `spc700.rs` | 1908 | 1911 |
| `dsp.rs` | 692 | 695 |
| `mod.rs` | 547 | 556 |
| `voice.rs` | 90 | 92 |
| `echo.rs` | 125 | 125 |

| # | Difference | snes | gb |
|---|---|---|---|
| D1 | Clock unit and `SPC_ACC_BIAS` | master-clock ticks, `96 * MASTER_CLOCK_HZ` | SPC cycles, `96` |
| D2 | Voice `brr` address | `u32` (`Addr`), saved with `w.u32` | `u16`, saved with `w.u16` |
| D3 | RAM in the state | host `StateWriter::bytes` writes raw (snes `src/state.rs:115-118`) | host `StateWriter::bytes` writes a u32 length prefix first (gb `src/state.rs:112-115`); the APU call sites are identical (`w.bytes(&*self.ram)`) |
| D4 | Shared constants | taken from the crate root | declared locally |
| D5 | Signed DSP state words | saved with `w.i16` (snes `src/apu/dsp.rs:586-591`) | re-encoded through `w.u16(v as u16)` (gb `src/sgb/spc/dsp.rs:589-594`; gb's writer has no `i16`); the bytes agree, both little-endian two's complement (snes `src/state.rs:96-98`) |

### 4.2 What tendy-spc takes

tendy-spc takes **snes's version**, for two reasons:

- It is the full SNES APU, the harder of the two users of the chip. gb's copy declaring snes's constants locally
  (D4) shows the code started in snes.
- Its direct suite is larger (snes `tests/spc700.rs` 32, `tests/dsp.rs` 20, `tests/apu.rs` 12, `tests/apu_ports.rs` 3;
  gb `tests/sgb_spc.rs` 36 and `tests/sgb_sound.rs` 22 test SGB use and stay in tendy-gb).

The two host differences become explicit parameters:

```rust
/// Which host drives the SPC, and so which clock units and state layout apply.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Host {
    /// Super NES: ticks are master clocks; state writes `brr` as u32 and RAM with no length prefix.
    Snes,
    /// Super Game Boy sound: ticks are SPC cycles; state writes `brr` as u16 and RAM with a u32 length prefix.
    Sgb,
}
```

- **Timing.** `Apu::new(host)` stores the host. The run loop uses the host's bias, so each host keeps today's arithmetic.
- **State.** The shared `save_state`/`load_state` are generic over a small write/read pair of traits implemented by
  each host's own `StateWriter`/`StateReader` — the traits hold only the methods both hosts already have, so there
  is no `i16` in them (gb's writer has none). Signed DSP words are written `w.u16(v as u16)` on both hosts: the same
  two little-endian two's-complement bytes as snes's `w.i16` today (snes `src/state.rs:96-98`; gb's copy already
  re-encodes this way, gb `src/sgb/spc/dsp.rs:589-594`). The `brr` width branches on the host (D2). RAM goes through
  the host's own `bytes`: gb's writes a u32 length prefix (gb `src/state.rs:112-115`), snes's raw
  (snes `src/state.rs:115-118`), while the APU call sites are identical (`w.bytes(&*self.ram)`) — no call-site
  branch there. Each host's state bytes stay byte-identical to its source.
- **`brr` width.** The internal type is snes's `u32`, and the SGB host writes `as u16`.
  - UNVERIFIED: that snes never holds a `brr` value above `0xFFFF` (SPC RAM is 64 KiB, so the address should wrap).
  - Settle in #6 with a test that runs the SNES pin ROM and asserts `brr <= 0xFFFF` after every voice step. If it
    fails, the SGB host masks to 16 bits exactly where gb's `u16` wrapped.
- **Paths stay the same.** Both hosts keep their old module path as a re-export:
  - tendy-snes: `pub mod apu` re-exports tendy-spc's modules;
  - tendy-gb: `sgb::spc` re-exports `tendy_spc`.

  So the existing tests compile unchanged and stay where they are.

### 4.3 Proof, and the fallback

#6 is done when all of these pins are equal to their pre-#6 values (§5.6):

- the SNES audio hash, frame checksums and state bytes;
- the SGB-sound audio hash and state bytes.

UNVERIFIED: that the audio stays identical. The bias is a scaled constant, and integer rounding at the scale boundary
could differ. Only the pins settle it.

If a pin differs and cannot be fixed inside tendy-spc, this is an **owner decision** (§9 Q1):

- (a) Keep gb's copy inside tendy-gb for v0.1. tendy-spc then serves SNES only, and this breaks SPEC §11's "shared
  SPC crate" until a later release.
- (b) Accept the new output, with a GB state version bump (GBST 10 → 11) and a loader branch for version 10.

Recommendation: (a). It keeps principle 3 (import first, improve later).

---

## 5. Import plan per core (#5–#10)

### 5.1 Rules for every import

1. **Copy, not rewrite.** The files are copied as they are at the source commit, keeping their history in the commit
   message only ("imported from <project> <commit>"). The source's git history is not imported (SPEC §11: fresh
   public history).
2. **A core crate takes the emulation modules plus their closure.** The closure means every module those files
   `use`. App-side modules, and anything that uses macroquad, cpal or rfd, stay in the source until an M4/M5 issue
   ports them.
3. **Allowed edits:**
   - module paths and `pub` visibility;
   - the new `src/machine_impl.rs` (§2.10);
   - re-exports;
   - `cargo fmt`, whitespace only.

   Nothing else. Any other edit is a separate issue after the inertness test passes.
4. **Tests follow the module they `use`.** A test file that uses only core items moves with the core. A test file
   that uses any app-side item waits for the issue that ports that module.
5. **Counts are gated per source file.** #5 records, for each source test file, the passed and ignored counts that
   `cargo test` reports at the source commit, in `baselines/test-counts.toml`. An import is done when every file it
   moved reports the same numbers.
6. **Never build in the source folders.** #5 clones each source commit into a temporary folder and runs the probes
   and `cargo test` there.

Test inventory at the source commits (counted with `grep` over `#[test]` and `#[ignore]`; `cargo test` numbers come from #5):

| Source | Integration `#[test]` | Integration `#[ignore]` | Unit tests in `src/` | Notes |
|---|---|---|---|---|
| gb | 837 | 38 | yes (e.g. `config` 11, `gba/bus` 33, `gba/cpu` 24, `status_bar` 15) | last full run at the source: 1072 passed, 0 failed, 30 ignored |
| nes | 1050 | 14 | yes | one binary `tests/it/main.rs` (about 90 modules) + 4 standalone binaries. The 14 `#[ignore = ...]` attributes: 6 in `tests/it` (`alignment_matrix`, `mapper228`, `nmi_race`, 3 in `tasvideos_movies`), 3 in `jit_perf`, 5 in `ppu_perf`; the other 4 of the 18 grep mentions in `tests/` are prose comments |
| snes | 373 | 95 | none | 47 test files |

### 5.2 tendy-gb (#7)

| Group | Files | Where |
|---|---|---|
| Emulation | every `src/` file except `src/gba/` and the app-side list below: CPU, PPU, APU, bus, cartridge and mappers, CGB, SGB (commands, borders, sound, `sgb/spc/` until #6), boot ROM, timer, serial, joypad, `machine.rs`, `system.rs`, `state.rs` | `crates/tendy-gb/src/` |
| Closure the core imports | `cheats.rs` (`bus.rs:8`, `system.rs:20`), `palette.rs` (`cgb.rs:414`), `debugger.rs` (`machine.rs:7`, `system.rs:21`), `save.rs` (`system.rs:578`, `:582`), `checksum.rs`, `link.rs` | `crates/tendy-gb/src/` |
| GBA types the gb trait names | `EffectState` (from `src/gba/mod.rs`) and `SwiCall` (from `src/gba/bios.rs`), for `system.rs:425` and `:441` | `crates/tendy-gb/src/gba_types.rs`; tendy-gba re-exports them |
| App-side (ported later) | `main.rs`, `audio.rs`, `config.rs`, `display.rs`, `draw.rs`, `file_pick.rs`, `gamepad.rs`, `keys.rs`, `launch.rs`, `menu.rs`, `recents.rs`, `rewind.rs`, `record.rs`, `screenshot.rs`, `session_switch.rs`, `settings.rs`, `status_bar.rs`, `thumb.rs`, `timing.rs`, `filter.rs` | §6.1 |
| Replaced | `launch.rs` `load_zip`/`load_7z` (by #14) | — |
| Dropped | `scripts/compare-acid2.py`, `scripts/compare-cgb-acid2.py` (the acid2 tests already compare pixels); `scripts/sweep-library.py` (replaced by #28); `scripts/glyph-words.py`, `scripts/png-colours.py`, `scripts/sweep-swi.sh` (developer probes); the examples (developer probes, about 33) | kept in the source archive (#35) |
| Merged by #5 | `scripts/fetch-test-roms.sh`, `scripts/test-roms.txt` | `scripts/` |

Notes:

- UNVERIFIED: the closure files import no app-side module. Settle: `cargo build -p tendy-gb` in #7. If one does, the
  smallest piece moves, and the move is recorded as a DESIGN amendment.
- UNVERIFIED: `EffectState` and `SwiCall` are plain data with no GBA-internal fields. Settle at the #7 build.
- UNVERIFIED: no test, script or doc runs an example. Settle with `git grep -n "examples/"` at the gb commit.
- **Link stays local.** It is `std::net` TCP on this machine only: the listener binds `127.0.0.1`
  (gb `src/link.rs:248`, the `TcpListener::bind(("127.0.0.1", port))` line; :247 is `pub fn tcp_host`) and the
  client connects to `127.0.0.1` (`:300`). This is consistent with SPEC's offline rule.
- **gb tests.** Every `tests/*.rs` file except the `gba_*` files and the ones that `use` app-side modules. Both
  acid2 tests stay pixel-exact; that check is gated by the moved acid2 tests themselves (`tests/acid2.rs`, 2 run, 3 ignored).

### 5.3 tendy-gba (#8)

| Group | Files | Where |
|---|---|---|
| Emulation | all of gb `src/gba/` (minus the two types moved by #7) | `crates/tendy-gba/src/` |
| Uses from tendy-gb | `link`, `state`, `serial`, `joypad`, `checksum`, `palette`, `cheats`, `cgb`, `debugger` (made `pub` by #7) | dependency |
| Tests | gb `tests/gba_*.rs` (e.g. `gba_bios` 60 run, 4 ignored; `gba_video` 30 run, 4 ignored) and the unit tests inside `src/gba/` (`bus` 33, `cpu` 24, ...) | `crates/tendy-gba/tests/`, in place |

### 5.4 tendy-nes (#9)

| Group | Files | Where |
|---|---|---|
| Core | `nes.rs`, `cart.rs`, `unif.rs`, `nsf.rs`, `mapper/*` (32 files incl. `fds.rs`), `cpu/*`, `ppu/*` except `ppu/overlay.rs`, `apu/*` except `apu/eq.rs` and `apu/overlay.rs` (incl. `apu/expansion`, VRC7 OPLL using libm), `controller.rs`, `console.rs`, `state.rs`, `checksum.rs`, `gamedb.rs` | `crates/tendy-nes/src/` |
| Tools that are core-side (no front imports) | `cdl`, `cheats`, `genie`, `cheatdb`, `watch`, `debugger`, `trace`, `profiler`, `symbols`, `events`, `movie` (BizHawk import uses zip), `history`, `rewind`, `runahead`, `tasedit`, `ra`, `patch`, `hdpack` (png), `ntsc` | `crates/tendy-nes/src/` |
| Split | `launch.rs` (3186 lines): the code that builds a `Nes` from bytes moves; CLI, config and display code (`launch.rs` uses `config` at line 10 and `display` at line 12) stays app-side. `save.rs` (396 lines): `battery_snapshot` (`:38`) and the body half of `load_save` (`:53`) move; path handling stays app-side | `crates/tendy-nes/src/loader.rs`, `save.rs` |
| App-side (ported later) | `main.rs`, `memview`, `history_view`, `audio`, `timing`, `avi`, `screenshot`, `crt`, `shader`, `display`, `config`, `settings`, `menu`, `status_bar`, `keys`, `gamepad`, `file_pick`, `recents`, `session_switch`, `draw`, `apu/eq.rs`, `apu/overlay.rs`, `ppu/overlay.rs` | §6.1 |
| Dropped | the other eight `tools/` files (every `tools/` file except `fetch_test_roms.py`): the task-runner scripts `step.py` and `qwen_task.py`, the fetch/reference probes `fetch_cheats.py`, `fetch_sst.py`, `oam2_ref_model.py`, `volume_reference.py`, `release-local.sh`, and `third_party_licenses.py` — its job (licence texts of every linked crate, generated per release) is #32's `scripts/notices.sh` | — |
| Merged by #5 | `tools/fetch_test_roms.py` (209 sha256-pinned entries) | `scripts/` |
| Decided by #34 | `.github/workflows/accuracy.yml` | — |

Notes:

- nes's `lib.rs` declares every module today, so macroquad and rfd are library dependencies. tendy-nes's `lib.rs`
  declares only the rows above. No core or tool file references a front module (nes inventory).
- UNVERIFIED: the exact line split of `launch.rs`. Settle by building `tendy-nes` with no front module. Every item
  the build asks for is either a core item (it moves) or a front item (the code that names it stays app-side).
- **nes tests.**
  - `tests/it/main.rs` and its modules that `use` only core and tool items go to `crates/tendy-nes/tests/it/`.
  - The four standalone binaries (`jit_perf`, `ppu_perf`, `ntsc_alloc`, `save_state_alloc`) and the test data files
    (`accuracycoin_baseline`, `game_snapshots*`, `tasvideos_*`, `sst_access_ok`) move too.
  - `ACCURACYCOIN_WRITE_BASELINE` keeps its meaning.
  - Unit tests inside app-side files (for example `main.rs:5121`) move with their file.

### 5.5 tendy-snes (#10)

| Group | Files | Where |
|---|---|---|
| Core | `lib.rs` constants, `cart/*` (header, map, chipset, refusal), `cpu/*`, `ppu/*`, `apu/*`, `dma`, `joypad.rs`, `machine.rs`, `state.rs`, `system.rs`, `picture.rs`, `checksum.rs`, and the rest of `src/` not listed below | `crates/tendy-snes/src/` |
| App-side (ported later) | `main.rs`, `audio.rs`, `config.rs`, `display.rs`, `draw.rs`, `file_pick.rs`, `gamepad.rs`, `keys.rs`, `launch.rs`, `menu.rs`, `recents.rs`, `save.rs`, `screenshot.rs`, `session_switch.rs`, `settings.rs`, `status_bar.rs`, `thumb.rs`, `timing.rs` | §6.1 |
| Front tests (move with the app modules) | `tests/audio_stats.rs`, `config.rs`, `display.rs`, `front_end.rs`, `launch.rs`, `menu.rs`, `save.rs`, and `audio_path.rs` if it uses `audio` | app, by the porting issue |
| Core tests | the other 39 or 40 `tests/*.rs` files | `crates/tendy-snes/tests/` |
| Dropped | `.design/*` and `docs/*` (snes's own design notes; TendyEmu has its own); `scripts/sweep.sh` (replaced by #28) | kept in the source archive |
| Root files | `rustfmt.toml` → workspace root (#4); `scripts/fetch-test-roms.sh`, `scripts/test-roms.txt` → merged by #5 | — |

Notes:

- `cart::header::score` and `cart::header::parse` become reachable as `tendy_snes::cart::header::*` for #12.
- The refusal names are unchanged: the strings in `cart/refusal.rs` are not touched, and #5 pins them (§5.6).
- `tests/` holds one file whose name carries an internal id (a halt probe). #11 renames it to `tests/halt_probe.rs`.

### 5.6 What the inertness harness pins (#5)

How the pins are taken:

- **Probes.** `tools/pin/{gb,gba,nes,snes}.rs` is one probe per source. Each uses only that source's own API:
  - gb: the `System` trait;
  - snes: the `System` trait;
  - nes: `struct Nes`.
- **Where they run.** #5 copies each probe into a temporary clone of the source commit as an example, and runs it
  in release mode there. The output goes to `baselines/<system>.pins` (format: `pin::parse`).
- **Where they are checked.** After each import, `crates/<core>/tests/inertness.rs` runs the same steps on the
  imported core and compares every pin.
- **ROMs.** They are found through the environment variable `TENDY_TEST_ROMS` (a folder), never a path in the
  repo. A run whose ROM is missing reports "skipped: <rom> not in TENDY_TEST_ROMS" and passes. CI and contributors
  without the file still see green, and the owner's run checks it.

| Run | ROM | What is pinned |
|---|---|---|
| `gb/dmg-acid2` | dmg-acid2 v1.0 (public, manifest) | frame checksum at frames 60, 300, 600 |
| `gb/cpu_instrs` | blargg cpu_instrs (public) | frame checksum at 600, 3000; audio hash over 3000 frames; state bytes (fnv1a64 + length) at 3000 |
| `gb/cgb-acid2` | cgb-acid2 v1.1 (public) | frame checksum at 60, 300, 600; state at 600 |
| `gb/sgb` | dmg-acid2 run in SGB mode | frame checksum with and without border at 600; state at 600 |
| `gb/sgb-sound` | the user's SGB cartridge file (local only) + a public GB ROM | audio hash over 1200 frames; state at 1200 |
| `gba/minimal` | a GBA image built in code by the probe: header, then mode 3 pixel and sound register writes in a loop | frame checksum at 60, 600; audio hash; state at 600 |
| `nes/cart` | a public blargg NES ROM from the manifest (#5 picks the first manifest entry with sound) | frame checksum (fnv1a64 of `framebuffer ‖ emphasis`) at 60, 600, 3000; audio hash; state at 3000 |
| `nes/nsf` | an NSF built in code (a play routine writing pulse registers) | audio hash over 600 frames |
| `nes/fds` | the user's FDS BIOS + a public FDS homebrew (local only) | frame checksum at 600; state at 600 |
| `snes/test-rom` | snes-test-roms v20210217 (public; #5 picks one with non-silent audio) | frame checksum at 60, 600, 3000; audio hash; state at 3000 |
| `snes/refusals` | headers built in code, one per `$FFD6` value 0..=255 | the exact refusal text, or "ok" |
| `*/roundtrip` | every run above | `load_state(save_state())` and then 60 more frames give the same frame checksum as running on without it |

Definitions:

- **Audio hash.** `set_sample_rate(48000)`, then drain after every frame into a 4096-frame buffer. The hash is fnv1a64
  over the drained samples as little-endian `f32` bytes, in order.
- **NES audio hash.** NES has no stereo drain, so its probe hashes each frame's `apu.samples` and then
  `apu.samples_right`. The wrapper's `drain_audio` interleaves them; that is tested separately in `nes_drain_interleaves`.
- **Test counts.** #5 also writes `baselines/test-counts.toml`: per source and per test file, the `passed` and
  `ignored` counts from `cargo test --release` at the source commit.

---

## 6. Front end (#15–#24)

### 6.1 Modules of `app/tendyemu/src/`

Which source each shared module comes from:

- **gb is the default origin.** gb's front end already runs two cores (GB and GBA) through one trait object, so it
  is the closest to a multi-core shell.
- **nes or snes win only where they hold the original, or the better fit.**

macroquad, cpal and rfd appear only in `main.rs`, `draw.rs`, `file_pick.rs`, `keys.rs` and the device half of
`audio.rs`. Everything else builds and tests headless.

| Module | Origin | Why this origin | Owner |
|---|---|---|---|
| `main.rs` | snes `main.rs` (1389 lines) as the skeleton | the smallest main of the three; snes already has a `System` trait | #15 |
| `app.rs` | new | `App`: the running `Box<dyn Machine>`, settings, hooks. No macroquad, so tests run headless | #15 |
| `loader.rs` | new | `tendy_detect::open` → the constructors of §2.10 | #15 |
| `menu.rs` | gb `menu.rs` | already drives two cores through one trait object | #15 |
| `status_bar.rs` | gb `status_bar.rs` | status lines, refusal lines, notices | #15 |
| `recents.rs` | gb `recents.rs` | adds the system key to each line | #15 |
| `file_pick.rs` | gb `file_pick.rs` | the filter becomes every SPEC §3 extension | #15 |
| `draw.rs` | gb `draw.rs` | | #15 |
| `timing.rs` | gb `timing.rs` | speeds, run mode, pacer; the rate comes from `Machine::frame_rate` | #15 |
| `battery.rs` | new, from the three `save.rs` | one battery writer (temporary file, then rename), each system's file extension as today | #15 |
| `caps.rs` | new | §2.3 | #16 |
| `keys.rs` | gb `keys.rs` | the default origin, and the smallest (199 lines); one file per system (§6.3) | #17 |
| `gamepad.rs` | gb `gamepad.rs` | the default origin: XInput loaded by hand through kernel32, no crate. NES ports 2–4 are added by #26 | #17 |
| `display.rs` | gb `display.rs` | already sizes GB, SGB-border and GBA frames; NES and SNES sizes added | #18 |
| `audio.rs` | gb `audio.rs` | `negotiate_rate` (never panics), the ring, and a sinc converter from any core rate | #19 |
| `switch.rs` | new | the three `session_switch.rs` only build the command line for a replacement session; TendyEmu swaps the core in-process (§6.4) | #20 |
| `config.rs` | gb `config.rs` (hand-written parser) | no new dependency (§1.3); rules in §6.2 | #21 |
| `settings.rs` | gb `settings.rs` | the default origin | #21 |
| `paths.rs` | gb `config_dir` (`config.rs:522`) | snes uses the same rule (`config.rs:131`) | #21 |
| `games.rs` | new | per-game overrides (SPEC §6) | #21 |
| `states.rs` | new, over the container (§2.7); slot naming as each app has it today | | #22 |
| `thumb.rs` | gb `thumb.rs` | PNG thumbnail beside each slot | #22 |
| `first_run.rs` | new | §6.5 | #23 |
| `rewind.rs` | gb `rewind.rs` | ported to use only `Machine::save_state`/`load_state`, so it works for any core | #24 |
| `screenshot.rs` | gb `screenshot.rs` | the size comes from the caller | #24 |
| `record.rs` | gb `record.rs` | the formats gb records today | #24 |
| `avi.rs` | nes `avi.rs` | NES users record AVI today (parity) | #24 |
| `headless.rs` | new | `tendyemu --headless --frames N <file>`; still one binary | #28 |
| `update.rs` | new | §9 Q9 | #36 |
| `gb/` | gb app-side features: palette choice, `filter.rs`, link UI, debugger panel, cheats UI, SGB options, boot ROM | parity | #25 |
| `nes/` | nes app-side features: `memview`, `history_view`, `crt`, `shader`, `apu/eq`, both overlays, movie and TAS UI, run-ahead, HD packs, RA, cheats UI, palettes, Vs. coins and DIP switches, NSF player, disk swap, Zapper and other devices, ports 2–4 | parity | #26 |
| `snes/` | snes app-side rows not covered by the shared shell | parity | #27 |

How modules connect without editing each other:

- #15 creates every module file above as a minimal working stub, with these hooks:
  - `pub fn rows(app: &App) -> Vec<Row>`;
  - `pub fn on_frame(app: &mut App)`, where the module needs it.
- `menu.rs` and `main.rs` call the hooks in a fixed order that #15 writes.
- After #15 merges, each file belongs to its owner, and only that owner edits it.
- Settings reach a module through `App::setting(system, key)`. The #15 stub returns `None`, which means "default";
  #21 makes it real.

### 6.2 Settings schema (`config.toml`)

Parsing rules: a missing key takes its default; an unknown key gives one notice and is then ignored.

Key names inside the system sections below are the ones this design relies on. Where a source config already has that
setting, the source's key name replaces the one shown here (U14).

```toml
[app]                      # shared by every system
scale = 3                  # integer window scale
fullscreen = false
volume = 100               # 0-100
mute = false
audio_rate = 0             # Hz asked of the device; 0 = device default (gb negotiate_rate)
rewind_seconds = 10
status_bar = true
last_folder = ""           # written by the app: where the file picker opens

[gb]                       # every key of gb's config.toml today, same names and values
console = "auto"           # auto | dmg | cgb | sgb  (gb's existing key; Auto never picks SGB)
boot_rom = ""              # file of a user boot ROM; empty = none
sgb_sound_rom = ""         # the user's SGB cartridge file; empty = no SGB sound

[gba]
bios = ""                  # optional user BIOS file

[nes]                      # every key of nes's config.toml today, same names and values
fds_bios = ""              # the user's 8192-byte FDS BIOS; empty = search as today

[snes]                     # every key of snes's config.toml today, same names and values
```

| Rule | Meaning |
|---|---|
| One section per system | `[gb]` covers DMG, CGB and SGB, because one core runs all three |
| Source keys keep their names | so #23 copies an old file into its section without renaming |
| A system section may repeat any `[app]` key | that value wins for that system (for example a larger `scale` for GB). #23 writes each old app's shared keys this way, so nothing is lost and no app's value has to win |
| Per-game file `<system>/games/<crc32>.toml` | holds keys of its system section; they win over the section |
| Order | per-game, then system section, then `[app]`, then the built-in default |
| Writes | atomic (write a temporary file, then rename) |

UNVERIFIED: the exact key lists of the three source configs. Settle in #21: list every key of each source
`config.rs` into the section's table in the #21 PR, with its default.

UNVERIFIED: whether gb's parser understands `[section]` lines. Settle in #21. If it does not, the section split is
added there (a few lines).

### 6.3 Folder layout

```
<config folder>/tendyemu/      Windows: %APPDATA%\tendyemu
                               Linux:   $XDG_CONFIG_HOME/tendyemu, else ~/.config/tendyemu
  config.toml                  §6.2
  recents.txt                  <system key> TAB <file>, newest first, 10 entries
  first-run.done               written when the first-run offer has been answered (#23)
  gb/    keys.txt   games/   saves/   states/   screenshots/   recordings/   cheats/
  gba/   keys.txt   games/   saves/   states/   screenshots/   recordings/
  nes/   keys.txt   games/   saves/   states/   screenshots/   recordings/   cheats/   movies/
  snes/  keys.txt   games/   saves/   states/   screenshots/   recordings/
```

The folder rule is the one gb and snes already use (gb `config.rs:520-528`, snes `config.rs:131`). #21 creates the
folders on a fresh install.

Every per-ROM file lives under its system's folders (owner decision, Q7 — per-system folders, SPEC §7), keyed by
the ROM's file stem, so one game's four kinds of file sit together and nothing is written beside a ROM:

| Kind | Path | Owner |
|---|---|---|
| battery save | `<system>/saves/<stem>.sav` (extension as today per system: snes `.srm`, nes `.sav`, gb as today) | #22 |
| save state | `<system>/states/<stem>.<slot>.st` + `<stem>.<slot>.png` thumbnail beside it | #22 |
| screenshot | `<system>/screenshots/<stem>-<n>.png` | #24 |
| recording | `<system>/recordings/<stem>-<n>.{wav,gif,avi}` | #24 |
| cheats (nes) | `<system>/cheats/<stem>.cht` | #26 |
| movies (nes) | `<system>/movies/<stem>-<n>.fm2` | #26 |

**Reading a save the old apps wrote.** On open of a ROM, if the per-system file is absent and a file of the same
stem and extension exists beside the ROM, that one is read and, on the first write, copied into the per-system
folder (never modified, never moved). A user who skips the #23 offer, or whose ROM never appeared in an old recents
list, therefore keeps their progress. Test: `save_beside_rom_adopted_on_first_write` (#22, #23).

UNVERIFIED: the gb battery extension and the gb/snes state slot names. Settle in #22 by reading each source's
`save.rs`/`thumb.rs`. Whatever they are, they are kept.

UNVERIFIED: where each app writes screenshots and recordings. Settle in #24, and keep the same place.

### 6.4 System switch sequence (#20)

Opening a file while a game runs, whether or not it is the same system:

| Step | Action | Source |
|---|---|---|
| 1 | Detect and build the new core (`loader.rs`). On refusal, show the refusal and keep the running game: stop here | new |
| 2 | Pause the audio callback. The stream stays open and plays silence (gb `fill_device_frames_or_silence`) | gb `audio.rs` |
| 3 | Old game: close any recording, stop any NES movie, write the battery (`battery_body` → atomic write), write its per-game settings, update recents | new |
| 4 | Drop the rewind ring, run-ahead state, and the old `Box<dyn Machine>` | new |
| 5 | New game: `load_battery_body(bytes, now_ms)`, then `tick_rtc` each frame; apply its system section, per-game file and key bindings; `set_sample_rate(device rate)`; rebuild the converter from `sample_rate()` | gb `system.rs:359`; gb `main.rs:970-975` (`tick_rtc`, `set_buttons`, `set_shoulders` each frame) |
| 6 | Resize the window to native size × scale (#18); read `capabilities()` again so the menu rows follow | new |
| 7 | Resume audio; the status bar shows `console_name()` | — |

Building before dropping costs one core's memory for a moment. In return, a refused file never ends the running game.

### 6.5 First-run copy (#23)

| Step | Action |
|---|---|
| 1 | Offer only when `first-run.done` is missing and an old app folder is found: `<config folder>/gb-rust/` or `<config folder>/snes-rust/`. nes keeps its `config.toml` beside its executable (nes `src/main.rs:71-73`), whose location TendyEmu cannot know, so the offer has a "Find the NES app folder" button |
| 2 | Copy: open each old file read-only (`config.toml`, `recents.txt`, keys files). Write the converted content into the new files (§6.2 sections, §6.3 folders). Never rename, move, change or delete an old file |
| 3 | Copy or skip, either answer writes `first-run.done`, so a second run does not offer |
| 4 | Battery saves and states for ROMs the old recents list point at are copied into `<system>/saves/` and `<system>/states/` (file beside the ROM, same stem). Never moved, never modified; a missing one is simply absent, and §6.3's adopt-on-first-write rule still catches the rest |

---

## 7. File ownership and order

### 7.1 Ownership rules

1. Each file has one owner issue: the one that creates or imports it.
2. Another issue may edit that file only if it depends on the owner (it runs later). Two issues that run in
   parallel never touch the same file.
3. Two files are shared by everyone:
   - `Cargo.lock`: on a conflict, take either side and regenerate it with `cargo metadata`.
   - Changelog: each PR writes `changelog.d/<issue>.md`, and #33 joins them. No PR edits a shared `CHANGELOG.md`.
4. #4 writes every `Cargo.toml` with all its dependencies (§1.2), so no later issue edits a manifest. The exception
   is #6: it adds `tendy-spc` to the tendy-gb and tendy-snes manifests.

### 7.2 Table

Lanes run side by side. A dash means the issue is on the critical path alone.

| Issue | Owns (sources) | Owns (tests) | Depends on | Lane |
|---|---|---|---|---|
| #4 | `Cargo.toml`, `Cargo.lock`, `rustfmt.toml`, `.gitignore`, every `crates/*/Cargo.toml` and `crates/*/src/lib.rs`, `crates/tendy-core/src/{state,pin}.rs`, `crates/tendy-detect/src/{signature,extension,archive}.rs` (stubs), `app/tendyemu/Cargo.toml`, `app/tendyemu/src/{main,lib}.rs` | `crates/tendy-core/tests/contract.rs`, `crates/tendy-detect/tests/api.rs` | #3 | — |
| #5 | `tools/pin/*`, `baselines/*`, `scripts/test-roms.txt`, `scripts/fetch-test-roms.sh`; body of `crates/tendy-core/src/pin.rs` | `crates/tendy-core/tests/pin_parse.rs` | #4 | — |
| #7 | `crates/tendy-gb/**` | `crates/tendy-gb/tests/**` | #5 | A |
| #8 | `crates/tendy-gba/**` | `crates/tendy-gba/tests/**` | #7 | A |
| #9 | `crates/tendy-nes/**` | `crates/tendy-nes/tests/**` | #5 | B |
| #10 | `crates/tendy-snes/**` | `crates/tendy-snes/tests/**` | #5 | C |
| #6 | `crates/tendy-spc/**`; edits `crates/tendy-snes/src/apu/*` and `Cargo.toml`, `crates/tendy-gb/src/sgb/spc/*`, `sgb/mod.rs` and `Cargo.toml` | `crates/tendy-spc/tests/layout.rs` | #7, #10 | C (after #7); parallel with #8, #9 |
| #11 | comment and name edits under `crates/tendy-{gb,gba,nes,snes,spc}/` only (imported code); renames the one snes test file whose name carries an internal id to `crates/tendy-snes/tests/halt_probe.rs` (§5.5) | `app/tendyemu/tests/scrub.rs` | #6, #8, #9, #10 | — |
| #12 | body of `crates/tendy-detect/src/signature.rs` | `crates/tendy-detect/tests/signature.rs` | #10 | D |
| #13 | body of `crates/tendy-detect/src/extension.rs` | `crates/tendy-detect/tests/extension.rs` | #4 | E (may start after #4) |
| #14 | body of `crates/tendy-detect/src/archive.rs` | `crates/tendy-detect/tests/archive.rs` | #12, #13 | D |
| #15 | `app/tendyemu/src/{main,app,loader,menu,status_bar,recents,file_pick,draw,timing,battery}.rs` + the stubs of every other M4 module | `app/tendyemu/tests/shell.rs` | #11, #13, #14 | — |
| #16 | `caps.rs` | `tests/capabilities.rs` | #15 | F |
| #17 | `keys.rs`, `gamepad.rs` | `tests/input.rs` | #15 | G |
| #18 | `display.rs` | `tests/display.rs` | #15 | H |
| #19 | `audio.rs` | `tests/audio.rs` | #15 | J |
| #20 | `switch.rs` | `tests/switch.rs` | #19 | J |
| #21 | `config.rs`, `settings.rs`, `paths.rs`, `games.rs` | `tests/settings.rs` | #15 | K |
| #23 | `first_run.rs` | `tests/first_run.rs` | #21 | K |
| #22 | `states.rs`, `thumb.rs`; body of `crates/tendy-core/src/state.rs` | `crates/tendy-core/tests/state_container.rs`, `tests/states.rs` | #15 | L |
| #24 | `rewind.rs`, `screenshot.rs`, `record.rs`, `avi.rs` | `tests/capture.rs` | #15 | M |
| #25 | `app/tendyemu/src/gb/**`, `baselines/dropped-tests-gb.txt` | `tests/gb_*.rs` | #16–#24 | N |
| #26 | `app/tendyemu/src/nes/**`, `baselines/dropped-tests-nes.txt` | `tests/nes_*.rs` | #16–#24 | O |
| #27 | `app/tendyemu/src/snes/**`, `baselines/dropped-tests-snes.txt` | `tests/snes_*.rs` | #16–#24 | P |
| #28 | `headless.rs`, `scripts/sweep.sh` | `tests/headless.rs` | #15 | Q |
| #29 | none (listening check, result recorded in the issue) | none | #6, #25 | — |
| #30 | `crates/tendy-detect/src/patch.rs`; edits `loader.rs` and the patch module in tendy-nes (it re-exports the shared one) | `crates/tendy-detect/tests/patch.rs` | #9, #14, #15 | R |
| #31 | `README.md` | none | #25, #26, #27 | S |
| #32 | `THIRD-PARTY-NOTICES.md`, `scripts/notices.sh` | `app/tendyemu/tests/notices.rs` | #25, #26, #27 | T |
| #34 | `.github/workflows/ci.yml`, or nothing if checks stay local | none | #28 | U |
| #36 | `update.rs` | `tests/update.rs` | #15 | V |
| #33 | `RELEASING.md`, `CHANGELOG.md` (joins `changelog.d/`) | none | #29–#32, #34, #36 | — |
| #35 | none (decision after release) | none | #33 | — |

Unless a path says otherwise, it is under `app/tendyemu/src/`, and test paths are under `app/tendyemu/tests/`.

```
#3 ─► #4 ─► #5 ─┬─► #7 ─┬─► #8 ───────────────┐
                │       └─► #6 (after #10) ───┤
                ├─► #9 ───────────────────────┼─► #11 ─┐
                └─► #10 ─┬────────────────────┘        │
                         └─► #12 ─► #14 ────────────────┼─► #15 ─► F G H J K L M Q V ─► N O P R ─► S T U ─► #33 ─► #35
          #4 ─► #13 ─────────────┴──────────────────────┘
```

---

## 8. Test list per issue

Two ways a test gets its owner:

- **Imported tests** keep their names and are owned by the issue that imports their file. Moving a file later
  (re-export, rename) does not change its owner.
- **New tests** are named below, once each.

| Issue | Imported suites | New tests |
|---|---|---|
| #4 | — | `contract.rs`: `system_tags_round_trip`, `system_keys_unique`, `legacy_magics_unique`, `buttons_bits_distinct`, `capabilities_with_has`, `fnv1a64_known_vectors` ("" → `cbf29ce484222325`, "a" → `af63dc4c8601ec8c`), `frame_is_complete_checks_length`, `state_refusal_texts_start_refused`. `api.rs`: `refusal_not_a_rom_text`, `refusal_no_rom_inside_text` |
| #5 | — | `pin_parse.rs`: `pin_parse_reads_runs_and_keys`, `pin_parse_rejects_line_without_equals`, `pin_files_parse` |
| #6 | — (snes `spc700`, `dsp`, `apu`, `apu_ports` and gb `sgb_spc`, `sgb_sound` stay with #10 and #7 through re-exports) | `layout.rs`: `snes_host_state_matches_snes_layout`, `sgb_host_state_matches_gb_layout`, `brr_stays_16_bit_on_snes_pin_rom` |
| #7 | every gb `tests/*.rs` file that uses only core items, and the unit tests in moved `src/` files (list: `baselines/test-counts.toml`, gb section) | `inertness.rs`: `gb_pins_dmg_acid2`, `gb_pins_cpu_instrs`, `gb_pins_cgb_acid2`, `gb_pins_sgb`, `gb_pins_sgb_sound`, `gb_pins_roundtrip`. `machine.rs`: `gb_machine_checksum_is_native`, `gb_buttons_map`, `gb_frame_rgba_complete`, `gb_state_blob_is_native`, `sgb_border_switches_frame_size` |
| #8 | gb `tests/gba_*.rs`; unit tests in `src/gba/` | `inertness.rs`: `gba_pins_minimal`, `gba_pins_roundtrip`. `machine.rs`: `gba_buttons_and_shoulders_map`, `gba_frame_rgba_complete` |
| #9 | nes `tests/it/*` modules that use only core and tool items; `jit_perf`, `ppu_perf`, `ntsc_alloc`, `save_state_alloc`; unit tests in moved `src/` files (incl. the accuracy suites: AccuracyCoin, blargg, TASVideos) | `inertness.rs`: `nes_pins_cart`, `nes_pins_nsf`, `nes_pins_fds`, `nes_pins_roundtrip`. `machine.rs`: `nes_buttons_map`, `nes_drain_interleaves`, `nes_channel_mask_mutes`, `nes_disk_side_swaps` |
| #10 | snes core test files (§5.5) | `inertness.rs`: `snes_pins_test_rom`, `snes_pins_refusals`, `snes_pins_roundtrip`. `machine.rs`: `snes_buttons_map`, `snes_frame_rgba_complete` |
| #11 | — | `scrub.rs`: `no_drive_paths_tracked`, `no_internal_ids_tracked`, `no_extra_patterns_tracked` (reads the local pattern file named by `TENDY_SCRUB_EXTRA`; passes when unset) |
| #12 | — | `signature.rs`: `signature_ines`, `signature_nes20`, `signature_unif`, `signature_fds_headered`, `signature_fds_raw`, `signature_nsf`, `signature_gba`, `signature_gba_bad_complement_no_match`, `signature_gb`, `signature_gb_cgb_flag`, `signature_gb_sgb_flag`, `signature_snes_lorom`, `signature_snes_hirom`, `signature_snes_exhirom`, `signature_snes_copier_header`, `snes_weak_header_not_a_signature`, `snes_scored_last`, `random_mib_refused`. Every image is built in code |
| #13 | — | `extension.rs`: `every_spec_extension_maps`, `extension_case_insensitive`, `fallback_logo_less_gb`, `fallback_raw_qd`, `fds_signature_with_qd_extension_is_qd`, `conflict_smc_holding_gb_opens_gb_with_notice`, `matching_extension_no_notice`, `unknown_extension_with_signature_no_notice`, `no_signature_unknown_extension_refused` |
| #14 | — | `archive.rs`: `zip_first_known_extension_wins`, `zip_header_match_without_known_extension`, `zip_without_rom_refused`, `sevenz_first_known_extension_wins`, `sevenz_header_match_without_known_extension`, `sevenz_without_rom_refused`, `nested_folder_entry_found`, `entry_over_cap_skipped`, `corrupt_archive_refused`. Archives are built in code (`zip::ZipWriter`, the sevenz-rust2 writer from its `compress` feature) |
| #15 | gb unit tests inside `menu.rs`, `status_bar.rs`, `recents.rs`, `file_pick.rs`, `draw.rs`, `timing.rs`; snes unit tests in `main.rs`, if any | `shell.rs`: `open_each_system_by_argument`, `open_each_system_from_menu_row`, `recents_mix_systems_and_reopen_right_core`, `refusal_shows_detect_text`, `battery_written_on_exit` |
| #16 | — | `capabilities.rs`: `capabilities_gb`, `capabilities_sgb_with_sound_rom`, `capabilities_gba`, `capabilities_nes_cart`, `capabilities_nes_disk`, `capabilities_nsf`, `capabilities_snes`, `menu_hides_rows_without_capability` |
| #17 | snes `keys.rs` unit tests; nes `gamepad.rs` unit tests | `input.rs`: `bindings_saved_per_system`, `bindings_restored_per_system`, `every_core_button_reachable_from_keyboard`, `gamepad_merges_with_keys` |
| #18 | gb `display.rs` unit tests | `display.rs`: `native_size_gb`, `native_size_sgb_border`, `native_size_gba`, `native_size_nes`, `native_size_snes`, `switch_resizes_window`, `integer_scale_fits` |
| #19 | gb `audio.rs` unit tests | `audio.rs`: `each_core_rate_converted_to_device`, `no_underrun_at_normal_speed_each_system`, `switch_keeps_stream_open` |
| #20 | nes `session_switch.rs` unit tests | `switch.rs`: `gb_snes_nes_gba_switch_writes_battery_each_time`, `refused_open_keeps_running_game`, `switch_drops_rewind_history`, `switch_closes_recording` |
| #21 | gb `config.rs` and `settings.rs` unit tests | `settings.rs`: `system_section_never_changes_another`, `fresh_install_creates_folders`, `missing_key_is_default`, `unknown_key_gives_one_notice`, `system_section_overrides_app_key`, `per_game_overrides_system_section` |
| #22 | gb `thumb.rs` unit tests | `state_container.rs`: `wrap_unwrap_round_trip_each_system`, `nes_state_refused_while_gb_runs`, `legacy_state_of_running_system_accepted`, `legacy_state_of_other_system_refused`, `newer_container_refused`, `truncated_container_refused`, `unknown_tag_refused`. `states.rs`: `own_states_round_trip_byte_identical_each_system`, `slot_files_named_as_source_apps`, `thumbnail_beside_slot` |
| #23 | — | `first_run.rs`: `copy_never_moves_or_changes_old_files`, `second_run_does_not_offer`, `old_settings_land_in_system_section`, `no_old_apps_no_offer`, `nes_folder_asked_when_not_found`, `battery_file_copied_into_system_folder` (all in a throwaway folder) |
| #24 | gb `rewind.rs`, `screenshot.rs`, `record.rs` unit tests; nes `avi.rs` unit tests | `capture.rs`: `rewind_steps_back_each_system`, `screenshot_native_size_each_system`, `wav_records_each_system`, `gif_records_each_system`, `avi_records_each_system` |
| #25 | every gb front test not taken above (ported into `src/gb/` or `tests/gb_*.rs`), or listed in `baselines/dropped-tests-gb.txt` with the test that replaces it | `gb_parity.rs`: `gb_parity_rows_reachable` |
| #26 | the same for nes (`tests/it` front modules, front unit tests) | `nes_parity.rs`: `nes_parity_rows_reachable` |
| #27 | the same for snes (front test files of §5.5) | `snes_parity.rs`: `snes_parity_rows_reachable` |
| #28 | — | `headless.rs`: `headless_runs_frames_each_system`, `sweep_counts_match_sources` (passes as skipped when `TENDY_ROM_LIBRARY` is unset) |
| #30 | — (nes patch tests stay with #9 through the re-export) | `patch.rs`: `ips_applies`, `ips_rle_record_applies`, `bps_applies`, `bps_bad_crc_refused`, `ups_applies`, `ups_bad_crc_refused`, `patch_beside_rom_found_each_system` |
| #32 | — | `notices.rs`: `notices_cover_every_lockfile_package` |
| #36 | — | `update.rs`: `newer_release_reported`, `same_release_up_to_date`, `no_curl_cannot_tell`, `unreadable_reply_cannot_tell` |
| #29, #31, #33, #34, #35 | — | none (listening check, documents, decisions) |

Gates that are not tests (every issue): `cargo build`, `cargo test`, `cargo fmt --check`,
`cargo clippy --all-targets -- -D warnings`, and no `todo!` or `unimplemented!`.

---

## 9. Risks and open questions

### 9.1 Open questions for the owner

Answered questions stay in the table with the owner's decision in the answer column; `docs/DECISIONS.md` lists the
decisions that changed what this document or the spec says.

| Q | Question | Recommendation |
|---|---|---|
| Q1 | If the shared SPC cannot keep SGB or SNES output byte-identical (§4.3), what wins? | Keep gb's copy in tendy-gb for v0.1, and share later. Inertness beats sharing |
| Q2 | Which SNES score counts as a signature? | 8 of 13 (§3.2), checked against the test sets in #12 |
| Q3 | Boundary frame: RGBA8888 or RGB555? | RGBA8888. NES palette tables are 8 bits per channel (nes `RgbaTable = [[u8; 4]; 512]`), and RGB555 would round them. Cost: 240 KiB per NES frame |
| Q4 | Accept tendy-gba depending on tendy-gb? | Yes for v0.1. Move the shared link/state code to tendy-core only if a later issue needs it |
| Q5 | Who ports the system-only front features (NES movies, TAS, run-ahead, HD packs, CRT/NTSC, shaders, RA, memview; GB link UI, debugger, LCD filter)? #15–#24 do not | #25, #26 and #27 become "port + parity checklist", each owning its `app/tendyemu/src/<system>/` folder. They run in parallel |
| Q6 | Front-end test counts cannot stay equal: three menus become one | Each dropped front test is listed per system with the test that replaces it (§8 #25–#27). The equal-count gate applies to core imports (#6–#10) |
| Q7 | Where per-ROM files (battery save, state, screenshot, recording, cheats, movies) live | **Decided by the owner 2026-11-26: per-system folders** under the config folder (SPEC §7), not beside the ROMs. §6.3 gives the paths and the adopt-on-first-write rule that keeps the old apps' saves working |
| Q8 | nes keeps its settings beside its executable | First-run offers a "Find the NES app folder" button (§6.5) |
| Q9 | #36 "Check for updates" versus SPEC's offline rule | A menu row only, never automatic. It runs `curl` from PATH (shipped with Windows 10 and later, and most Linux) against the public releases page. No new dependency. It reports "cannot tell" when curl is missing or the machine is offline |
| Q10 | The #11 scrub check in a public repo cannot list personal names | The tracked test holds generic patterns only (drive letters, home-folder paths, internal id shapes). Name patterns live in an untracked local file named by `TENDY_SCRUB_EXTRA` |
| Q11 | nes's scrub load is large (about 1104 `#NNN` refs, 78 `PR-` refs, a personal name in one test file, an internal tool script) | Drop the tool script, strip the ids, keep the technical reasons in the comments |
| Q12 | gb's about 33 examples are developer probes | Drop them from TendyEmu; they stay in the archived source |
| Q13 | Archive entry cap | 64 MiB. The largest supported images (GBA 32 MiB, SNES ExHiROM 8 MiB) fit with room |
| Q14 | Should `.sgb` or the `$0146` flag select the SGB model? | **Decided by the owner 2026-11-26: manual only.** SGB is a setting (the core already offers one); detection never selects it. The `.sgb`-by-header rule and auto-selection go to the backlog |
| Q15 | New states are containers, which the old apps cannot read | Accept: old states load in TendyEmu, and the old apps are archived (#35) |

### 9.2 Risks

| Risk | Effect | Mitigation |
|---|---|---|
| Hidden coupling in gb's closure files, or in the nes `launch.rs` split | the import issue grows | The build tells; the smallest move is recorded as a DESIGN amendment |
| Pins depend on test ROMs that are not in the repo | a contributor without the ROMs cannot check inertness | Public ROMs come from the manifest with sha256; local-only runs say "skipped" |
| Two checkouts sharing one `CARGO_TARGET_DIR` | cargo can run the other checkout's stale build, and tests pass on old code | Each worktree or clone uses its own target folder (#5 clones included) |
| `Cargo.lock` conflicts between lanes | merge friction | Regenerate; never merge by hand (§7.1) |
| Test ROM licences: nes's fetch script states its ROMs have unclear licences | they cannot be committed | Fetched or local only; never committed (§10) |
| Edition 2024 | needs Rust 1.85 or newer | State it in the README (#31) |

### 9.3 UNVERIFIED items (each with the check that settles it)

| # | Item | Settled by |
|---|---|---|
| U1 | tendy-spc keeps SNES and SGB audio byte-identical | #6: the pins of §5.6 |
| U2 | snes never holds `brr > 0xFFFF` | #6: `brr_stays_16_bit_on_snes_pin_rom` |
| U3 | SNES signature score 8 separates the test sets | #12: score every manifest image |
| U4 | gb closure files (`cheats`, `palette`, `debugger`, `save`, `checksum`, `link`) import no app-side module | #7: `cargo build -p tendy-gb` |
| U5 | `EffectState` and `SwiCall` are plain data | #7 build |
| U6 | exact line split of nes `launch.rs` and `save.rs` | #9: build tendy-nes with no front module |
| U7 | which source test files use app-side items | each import issue: the build, per file |
| U8 | gb and nes pace at the `frame_rate` values in §2.4 | read each source's `timing.rs` rate constant in #7 and #9 |
| U9 | the NES game database keys on CRC-32 of PRG‖CHR | #9: read nes `src/gamedb.rs` |
| U10 | the name of nes's console-settings type | #9: `git grep -n "pub struct" src/console.rs` |
| U11 | gb shows the LCD filter for GBA too; nes offers every NES row for NSF and FDS | #25, #26 checklists |
| U12 | the gb battery extension; gb and snes state slot names | #22: read each `save.rs`/`thumb.rs` |
| U13 | no gb test, script or doc runs an example | #7: `git grep -n "examples/"` |
| U14 | the exact config keys of the three sources; whether gb's parser reads `[section]` | #21 |
| U15 | where each app writes screenshots and recordings | #24 |
| U16 | gb's converter alone keeps every core free of underruns | #19: `no_underrun_at_normal_speed_each_system`; if SNES fails, port snes `audio.rs`'s rate handling |
| U17 | how a QD image differs from an FDS side beyond the shared first block | not needed for v0.1: QD stays extension-only (§3.3) |
| U18 | gb and nes `rustfmt` settings match snes's | #7, #9: `cargo fmt` gives a whitespace-only diff either way |
| U19 | snes never outputs 512-wide or 239-line pictures (C7) | #18: run a hi-res / overscan test ROM from the manifest and check `frame()` stays 256x224 |

---

## 10. Public-repo rule

This repository is public. Tracked files never contain:

| Never | Instead |
|---|---|
| Local paths: drive letters, home folders, a developer's ROM library | `TENDY_TEST_ROMS` (test ROM folder), `TENDY_ROM_LIBRARY` (#28 sweep), `TENDY_SCRUB_EXTRA` (local scrub patterns); user files through settings |
| Personal names | none needed |
| Internal job, card or PR ids from other projects, and private notes | plain technical reasons in comments |
| ROMs, BIOS files, SGB cartridge files, boot ROMs, user saves | manifests with public URLs and sha256 (`scripts/test-roms.txt`); test images built in code |
| Account names or URLs of the source repositories | "imported from <project> <commit>" in commit messages |

Where it is enforced:

- #11's `scrub.rs` checks every tracked file on every test run.
- The pin files hold hashes and counts only.
- This document follows the same rule. Paths in it are relative to the repository root or to a placeholder such as
  `<config folder>`.
