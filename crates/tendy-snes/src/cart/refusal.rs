//! §8.4's refusals and §10.3's two loader notices, worded in one place.
//!
//! Three callers quote these lines: the window's error screen and status bar (`src/main.rs`), the
//! headless `--frames` run, and §12.4's sweep. Every refusal begins with [`REFUSED_TAG`], and that
//! tag is the only pattern the sweep matches, so a refusal the loader grows cannot be filed as a
//! crash and there is no second list of names to keep in step.
//!
//! A refusal is a claim about the **cartridge**, not about the emulator: v1 runs ROM, ROM+RAM and
//! ROM+RAM+battery boards (§8.4), and a cartridge that asks for anything else is named and left
//! alone. A bad checksum, a PAL region code and an implausible size code are reported, not refused
//! (§8.2, §8.4), because a homebrew image with header errors still runs.

use super::chipset::Chipset;

/// The prefix every [`Refusal`] carries, and the only pattern `scripts/sweep.sh` classifies on.
pub const REFUSED_TAG: &str = "refused: ";

/// §10.3's notice for a PAL cartridge: v1 runs it at NTSC timing rather than refusing it (§8.4).
pub const PAL_NOTICE: &str = "PAL cartridge, NTSC timing";

/// §10.3's notice for a header whose complement and sum do not agree: reported, not refused (§8.2).
pub const CHECKSUM_NOTICE: &str = "header checksum bad";

/// Why the loader said no (§8.4).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// A named coprocessor board: `$FFD6` (subclassed by `$FFBF`) maps to a chip, and only the
    /// three ROM boards are run. The byte travels with the name so a report can quote both.
    Coprocessor { chipset: Chipset, byte: u8 },
    /// No candidate header scored high enough, or the image is one repeated byte — erased or
    /// zero-filled — which carries no header whatever the score says (§8.1).
    NoHeader { best_score: i32 },
    /// The image is smaller than the smallest licensed board, so it cannot hold vectors in any map.
    TooSmall { bytes: usize },
    /// The file, or the archive's entry, could not be read.
    Unreadable { path: String, why: String },
    /// A `.zip`/`.7z` with no `.smc`/`.sfc` entry (§10.1).
    NoCartridgeInArchive { path: String },
}

impl Refusal {
    /// The class this refusal belongs to: a stable short name, so a test or a report can group
    /// refusals the way §8.4 does without parsing the sentence.
    pub fn class(&self) -> &'static str {
        match self {
            Refusal::Coprocessor { .. } => "coprocessor",
            Refusal::NoHeader { .. } => "no header",
            Refusal::TooSmall { .. } => "too small",
            Refusal::Unreadable { .. } => "unreadable file",
            Refusal::NoCartridgeInArchive { .. } => "no cartridge in archive",
        }
    }
}

impl std::fmt::Display for Refusal {
    /// The one line: the tag, then what the cartridge is and why it is not running.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(REFUSED_TAG)?;
        match self {
            Refusal::Coprocessor { chipset, byte } => {
                write!(f, "{}, not in v1 ($FFD6 = ${byte:02X})", chipset.name())
            }
            Refusal::NoHeader { best_score } => {
                write!(
                    f,
                    "no cartridge header found (best score {best_score} of 13)"
                )
            }
            Refusal::TooSmall { bytes } => {
                write!(f, "ROM is {bytes} bytes, need at least 32768")
            }
            Refusal::Unreadable { path, why } => write!(f, "cannot read {path}: {why}"),
            Refusal::NoCartridgeInArchive { path } => {
                write!(f, "no .smc or .sfc entry in {path}")
            }
        }
    }
}
