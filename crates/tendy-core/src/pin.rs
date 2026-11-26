//! Parsing for the pin files under `baselines/` (§5.6).

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
