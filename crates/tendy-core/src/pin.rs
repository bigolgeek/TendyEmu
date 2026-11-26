//! Pin files: the output a source core produces, written down so an import can be checked
//! against it (DESIGN §5.6).
//!
//! A pin file is a flat text file. Lines are `[run]` headers, `key = value` pairs, blank lines,
//! or `#` comments. Values are kept exactly as written: a frame checksum is lower-case hex, a
//! state length is decimal, and a refusal is the sentence the core prints.

/// One `key = value` line from a pin file, and the run it belongs to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pin {
    /// The `[run]` header the line sits under.
    pub run: String,
    /// The measurement's name, e.g. `frame@600` or `audio`.
    pub key: String,
    /// What was measured, exactly as written.
    pub value: String,
}

/// Parses a pin file.
///
/// A `key = value` split happens at the first `=`, so a refusal text may itself contain one.
/// Every key must sit under a `[run]` header.
pub fn parse(text: &str) -> Result<Vec<Pin>, String> {
    let mut pins = Vec::new();
    let mut run: Option<String> = None;
    for (index, raw) in text.lines().enumerate() {
        let line = raw.trim();
        let line_no = index + 1;
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(rest) = line.strip_prefix('[') {
            let Some(name) = rest.strip_suffix(']') else {
                return Err(format!("line {line_no}: [run] header is missing its ]"));
            };
            let name = name.trim();
            if name.is_empty() {
                return Err(format!("line {line_no}: [run] header needs a name"));
            }
            run = Some(name.to_owned());
            continue;
        }
        let Some(current) = run.as_deref() else {
            return Err(format!(
                "line {line_no}: key = value before any [run] header"
            ));
        };
        let Some((key, value)) = line.split_once('=') else {
            return Err(format!("line {line_no}: expected key = value or [run]"));
        };
        let key = key.trim();
        let value = value.trim();
        if key.is_empty() {
            return Err(format!("line {line_no}: key is empty"));
        }
        if value.is_empty() {
            return Err(format!("line {line_no}: value is empty"));
        }
        pins.push(Pin {
            run: current.to_owned(),
            key: key.to_owned(),
            value: value.to_owned(),
        });
    }
    Ok(pins)
}
