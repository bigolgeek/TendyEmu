//! Extension fallback, conflict notice and refusal (SPEC §4 steps 3-5, DESIGN §3.3).

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
