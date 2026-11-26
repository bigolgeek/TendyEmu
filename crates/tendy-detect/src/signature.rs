//! Header signatures (SPEC §4 step 2, DESIGN §3.2).

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
