//! The save-state container (§2.7): magic, version, system tag, blob length, blob.

use super::{StateRefusal, SystemKind};

/// First four bytes of every container.
pub const MAGIC: [u8; 4] = *b"TNDY";
/// The container layout version this build writes.
pub const VERSION: u16 = 1;
/// Bytes before the blob: magic 4 + version 2 + tag 4 + blob length 4.
pub const HEADER_LEN: usize = 14;

/// Wraps a core's blob for `system`.
///
/// Panics if `blob` is longer than `u32::MAX` bytes, which no save state can be.
pub fn wrap(system: SystemKind, blob: &[u8]) -> Vec<u8> {
    let len = u32::try_from(blob.len()).expect("a save state blob is far smaller than 4 GiB");
    let mut out = Vec::with_capacity(HEADER_LEN + blob.len());
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&VERSION.to_le_bytes());
    out.extend_from_slice(&system.tag());
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(blob);
    out
}

/// Returns the blob inside `bytes` if it belongs to `running`. A raw state from an earlier
/// app (it starts with `running.legacy_state_magic()`) is returned whole.
pub fn unwrap(bytes: &[u8], running: SystemKind) -> Result<&[u8], StateRefusal> {
    if bytes.starts_with(&running.legacy_state_magic()) {
        return Ok(bytes);
    }
    for system in SystemKind::ALL {
        if system != running && bytes.starts_with(&system.legacy_state_magic()) {
            return Err(StateRefusal::WrongSystem {
                state: system,
                running,
            });
        }
    }
    if !bytes.starts_with(&MAGIC) {
        return Err(StateRefusal::NotAState);
    }
    let version = read_u16(bytes, 4).ok_or(StateRefusal::Truncated)?;
    if version > VERSION {
        return Err(StateRefusal::NewerContainer(version));
    }
    let tag: [u8; 4] = bytes
        .get(6..10)
        .ok_or(StateRefusal::Truncated)?
        .try_into()
        .expect("a four-byte slice converts to a four-byte array");
    let Some(state) = SystemKind::from_tag(tag) else {
        return Err(StateRefusal::NotAState);
    };
    if state != running {
        return Err(StateRefusal::WrongSystem { state, running });
    }
    let len = read_u32(bytes, 10).ok_or(StateRefusal::Truncated)? as usize;
    let end = HEADER_LEN.checked_add(len).ok_or(StateRefusal::Truncated)?;
    bytes.get(HEADER_LEN..end).ok_or(StateRefusal::Truncated)
}

/// The little-endian `u16` at `at`, if `bytes` holds one.
fn read_u16(bytes: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(bytes.get(at..at + 2)?.try_into().ok()?))
}

/// The little-endian `u32` at `at`, if `bytes` holds one.
fn read_u32(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}
