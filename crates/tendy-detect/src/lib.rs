//! SPEC §4: decide which core runs a file, or refuse it.

pub mod archive;
pub mod extension;
pub mod signature;

pub use archive::{ArchiveKind, Entry, open};

use std::fmt;
use tendy_core::SystemKind;

// The dependency graph is frozen by DESIGN §1.2; the detection modules (#12-#14) use these crates.
use sevenz_rust2 as _;
use tendy_snes as _;
use zip as _;

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
        Refusal {
            reason: format!("not a recognised ROM ({file})"),
        }
    }

    /// `refused: no ROM inside <archive>` (SPEC §4 step 1).
    pub fn no_rom_inside(archive: &str) -> Refusal {
        Refusal {
            reason: format!("no ROM inside {archive}"),
        }
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
