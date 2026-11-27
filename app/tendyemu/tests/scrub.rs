//! DESIGN §8 (#11): the public tree carries no private bookkeeping.
//!
//! Three gates over `git ls-files`: drive-letter and home-folder paths, internal job and card
//! ids, and a local extra list named by `TENDY_SCRUB_EXTRA`. Every test fails with the full list
//! of `file:line` hits. Binary files, `LICENSE*` and this file are skipped.

use std::path::{Path, PathBuf};
use std::process::Command;

/// A tracked file that reads as UTF-8 and is inside the scrub.
struct Tracked {
    path: String,
    text: String,
}

fn repo_root() -> PathBuf {
    let out = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .expect("git must be installed: the scrub test lists tracked files");
    assert!(
        out.status.success(),
        "git rev-parse --show-toplevel failed: {}",
        String::from_utf8_lossy(&out.stderr).trim()
    );
    PathBuf::from(
        String::from_utf8(out.stdout)
            .expect("git prints a UTF-8 toplevel")
            .trim(),
    )
}

/// This file's repo-relative path, so the gate does not flag its own patterns.
fn own_path(root: &Path) -> String {
    let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let package = Path::new(env!("CARGO_MANIFEST_DIR"));
    let package = package
        .canonicalize()
        .unwrap_or_else(|_| package.to_path_buf());
    let rel = package.strip_prefix(&root).unwrap_or(Path::new(""));
    rel.join("tests")
        .join("scrub.rs")
        .to_string_lossy()
        .replace('\\', "/")
}

fn tracked_files() -> Vec<Tracked> {
    let root = repo_root();
    let own = own_path(&root);
    let out = Command::new("git")
        .args(["ls-files", "-z"])
        .current_dir(&root)
        .output()
        .expect("git ls-files must run: the scrub test needs git");
    assert!(
        out.status.success(),
        "git ls-files failed: {}",
        String::from_utf8_lossy(&out.stderr).trim()
    );
    let mut files = Vec::new();
    for raw in out.stdout.split(|byte| *byte == 0) {
        let Ok(path) = std::str::from_utf8(raw) else {
            continue;
        };
        if path.is_empty() {
            continue;
        }
        // The suffix check keeps the test from scanning itself when the checkout moved after
        // compilation, which would make `own` stale.
        if path == own || path.ends_with("/tests/scrub.rs") {
            continue;
        }
        if path
            .rsplit('/')
            .next()
            .is_some_and(|name| name.starts_with("LICENSE"))
        {
            continue;
        }
        let Ok(bytes) = std::fs::read(root.join(path)) else {
            continue;
        };
        let Ok(text) = String::from_utf8(bytes) else {
            continue;
        };
        files.push(Tracked {
            path: path.to_string(),
            text,
        });
    }
    files
}

fn hit(file: &Tracked, number: usize, line: &str) -> String {
    format!("  {}:{}: {}", file.path, number, line.trim_end())
}

fn assert_no_hits(what: &str, hits: &[String]) {
    assert!(
        hits.is_empty(),
        "{what}: {} hit(s):\n{}",
        hits.len(),
        hits.join("\n")
    );
}

fn is_word(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

/// The drive letter must start a token: start of line, whitespace, or an opening delimiter.
/// An apostrophe only starts a token after a space, so a possessive such as `board's:\n` in a
/// format string stays out.
fn starts_a_path(bytes: &[u8], i: usize) -> bool {
    if i == 0 {
        return true;
    }
    let previous = bytes[i - 1];
    if previous == b'\'' {
        return i < 2 || !is_word(bytes[i - 2]);
    }
    matches!(
        previous,
        b' ' | b'\t'
            | b'\n'
            | b'\r'
            | b'('
            | b'['
            | b'{'
            | b'`'
            | b'"'
            | b'='
            | b'>'
            | b','
            | b';'
            | b'*'
    )
}

/// `:\n`, `:\t` and friends in a Rust string are escapes, not drive paths. `C:\temp` continues
/// with a word character and is still a path.
fn is_string_escape(bytes: &[u8], at: usize) -> bool {
    let Some(&escape) = bytes.get(at) else {
        return false;
    };
    let quoted = matches!(escape, b'n' | b'r' | b't' | b'0' | b'\\' | b'\'' | b'"');
    quoted && !bytes.get(at + 1).is_some_and(|next| is_word(*next))
}

/// True when `line` holds a drive-letter path (`C:\...`, `D:/...`).
///
/// Two shapes a bare letter-colon-slash match gets wrong are filtered out: a format string
/// ending a sentence (`"...:\n{}"`), and a flag list such as rcheevos's `A:/B:/N:`.
fn has_drive_path(line: &str) -> bool {
    let bytes = line.as_bytes();
    for i in 0..bytes.len().saturating_sub(2) {
        if !bytes[i].is_ascii_alphabetic()
            || bytes[i + 1] != b':'
            || (bytes[i + 2] != b'/' && bytes[i + 2] != b'\\')
        {
            continue;
        }
        if !starts_a_path(bytes, i) {
            continue;
        }
        if bytes[i + 2] == b'\\' && is_string_escape(bytes, i + 3) {
            continue;
        }
        if bytes[i + 2] == b'/'
            && bytes.get(i + 3).is_some_and(u8::is_ascii_alphabetic)
            && bytes.get(i + 4) == Some(&b':')
        {
            continue;
        }
        return true;
    }
    false
}

/// True when `line` holds a home-folder path: `C:\Users\...`, `/home/...` or `/root/...`.
fn has_home_path(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    if lower.contains("users/") || lower.contains("users\\") {
        return true;
    }
    for needle in ["/home/", "/root/"] {
        let mut from = 0;
        while let Some(at) = lower[from..].find(needle) {
            let index = from + at;
            if index == 0 || !is_word(lower.as_bytes()[index - 1]) {
                return true;
            }
            from = index + needle.len();
        }
    }
    false
}

/// True when `line` carries an internal id: `JOB-N`, `PR-N`, `t_` plus eight hex digits, a
/// `jobs/` folder or `REPORT.md`. Issue refs (`#12`) are this repo's own and stay.
fn has_internal_id(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    let bytes = lower.as_bytes();
    for prefix in ["job-", "pr-"] {
        let mut from = 0;
        while let Some(at) = lower[from..].find(prefix) {
            let index = from + at;
            let after = index + prefix.len();
            let bounded = index == 0 || !is_word(bytes[index - 1]);
            if bounded && bytes.get(after).is_some_and(|byte| byte.is_ascii_digit()) {
                return true;
            }
            from = after;
        }
    }
    for i in 0..bytes.len().saturating_sub(1) {
        if bytes[i] != b't' || bytes[i + 1] != b'_' {
            continue;
        }
        let bounded = i == 0 || !is_word(bytes[i - 1]);
        let card = bytes.get(i + 2..i + 10);
        let tail = bytes.get(i + 10);
        if bounded
            && card.is_some_and(|hex| hex.iter().all(|byte| byte.is_ascii_hexdigit()))
            && !tail.is_some_and(|byte| is_word(*byte))
        {
            return true;
        }
    }
    lower.contains("jobs/") || lower.contains("jobs\\") || lower.contains("report.md")
}

/// One line of the local extra list.
struct ExtraPattern {
    number: usize,
    needle: String,
    ignore_case: bool,
}

impl ExtraPattern {
    fn matches(&self, line: &str) -> bool {
        if self.ignore_case {
            line.to_ascii_lowercase().contains(&self.needle)
        } else {
            line.contains(&self.needle)
        }
    }
}

/// The local list is one pattern per line; `#` lines are comments. The workspace has no regex
/// crate, so this reader understands only a literal substring and a leading `(?i)`. Lines that
/// use other regex syntax are matched as literals and match nothing here; the private grep
/// covers those.
fn extra_patterns() -> Vec<ExtraPattern> {
    let Some(value) = std::env::var_os("TENDY_SCRUB_EXTRA") else {
        return Vec::new();
    };
    let path = PathBuf::from(value);
    if path.as_os_str().is_empty() {
        return Vec::new();
    }
    let text = std::fs::read_to_string(&path).unwrap_or_else(|error| {
        panic!("cannot read TENDY_SCRUB_EXTRA {}: {error}", path.display())
    });
    let mut patterns = Vec::new();
    for (number, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (needle, ignore_case) = match line.strip_prefix("(?i)") {
            Some(rest) => (rest, true),
            None => (line, false),
        };
        if needle.is_empty() {
            continue;
        }
        patterns.push(ExtraPattern {
            number: number + 1,
            needle: if ignore_case {
                needle.to_ascii_lowercase()
            } else {
                needle.to_string()
            },
            ignore_case,
        });
    }
    patterns
}

#[test]
fn no_drive_paths_tracked() {
    let mut hits = Vec::new();
    for file in tracked_files() {
        for (number, line) in file.text.lines().enumerate() {
            if has_drive_path(line) || has_home_path(line) {
                hits.push(hit(&file, number + 1, line));
            }
        }
    }
    assert_no_hits("drive-letter or home-folder paths are tracked", &hits);
}

#[test]
fn no_internal_ids_tracked() {
    let mut hits = Vec::new();
    for file in tracked_files() {
        for (number, line) in file.text.lines().enumerate() {
            if has_internal_id(line) {
                hits.push(hit(&file, number + 1, line));
            }
        }
    }
    assert_no_hits("internal job, card or report ids are tracked", &hits);
}

#[test]
fn no_extra_patterns_tracked() {
    let patterns = extra_patterns();
    if patterns.is_empty() {
        return;
    }
    let mut hits = Vec::new();
    for file in tracked_files() {
        for (number, line) in file.text.lines().enumerate() {
            for pattern in &patterns {
                if pattern.matches(line) {
                    // The source line is left out: it carries the private word itself.
                    hits.push(format!(
                        "  {}:{} [extra list line {}]",
                        file.path,
                        number + 1,
                        pattern.number
                    ));
                    break;
                }
            }
        }
    }
    assert_no_hits("patterns from TENDY_SCRUB_EXTRA are tracked", &hits);
}
