//! Extension fallback, conflict notice and refusal (SPEC §4 steps 3-5, DESIGN §3.3).

use tendy_core::SystemKind;

use crate::signature::Match;
use crate::{Basis, Detected, Format, Refusal};

/// The system an extension names (§3.3 table). Case-insensitive, without the dot.
pub fn system_of(ext: &str) -> Option<SystemKind> {
    format_of(ext).map(|(system, _)| system)
}

/// SPEC §4 steps 3-5: the extension fallback, the conflict notice and the refusal.
pub fn resolve(name: &str, by_signature: Option<Match>) -> Result<Detected, Refusal> {
    let ext = extension_of(name);
    let named = ext.and_then(format_of);

    let Some(sig) = by_signature else {
        return match named {
            Some((system, format)) => Ok(Detected {
                system,
                format,
                basis: Basis::Extension,
                notice: None,
                // `resolve` reads no bytes, so the header flags stay unread on this path (§3.4).
                gb: None,
                copier_header: false,
            }),
            None => Err(Refusal::not_a_rom(name)),
        };
    };

    // §3.3: a raw FDS side named `.qd` is a QD image; both names agree on the system.
    let format = match named {
        Some((_, Format::Qd)) if sig.format == Format::FdsRaw => Format::Qd,
        _ => sig.format,
    };
    // §3.3: the signature wins, and the notice names the extension and the system it opened as.
    let notice = match named {
        Some((system, _)) if system != sig.system => {
            Some(conflict_notice(ext.unwrap_or_default(), system, sig.system))
        }
        _ => None,
    };

    Ok(Detected {
        system: sig.system,
        format,
        basis: Basis::Signature,
        notice,
        gb: sig.gb,
        copier_header: sig.copier_header,
    })
}

/// The extension's system and the format assumed when no signature matched (§3.3 table).
fn format_of(ext: &str) -> Option<(SystemKind, Format)> {
    let named = match ext.to_ascii_lowercase().as_str() {
        "gb" | "gbc" | "sgb" => (SystemKind::Gb, Format::Gb),
        "gba" | "agb" => (SystemKind::Gba, Format::Gba),
        "nes" => (SystemKind::Nes, Format::Ines),
        "unf" | "unif" => (SystemKind::Nes, Format::Unif),
        "fds" => (SystemKind::Nes, Format::FdsRaw),
        "qd" => (SystemKind::Nes, Format::Qd),
        "nsf" => (SystemKind::Nes, Format::Nsf),
        "sfc" | "smc" | "swc" | "fig" => (SystemKind::Snes, Format::Snes),
        _ => return None,
    };
    Some(named)
}

/// The suffix after the last dot, or `None` (§3.3: matched on the last dot only).
fn extension_of(name: &str) -> Option<&str> {
    let (_, ext) = name.rsplit_once('.')?;
    (!ext.is_empty()).then_some(ext)
}

/// SPEC §4 step 4's notice: `file named .smc is a Game Boy ROM — opened as Game Boy`.
fn conflict_notice(ext: &str, named: SystemKind, opened: SystemKind) -> String {
    // "an NES ROM"; the other three names take "a".
    let article = if named == SystemKind::Nes { "an" } else { "a" };
    format!(
        "file named .{ext} is {article} {} ROM — opened as {}",
        named.display_name(),
        opened.display_name()
    )
}
