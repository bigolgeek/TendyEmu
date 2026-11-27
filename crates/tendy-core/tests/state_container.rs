//! The save-state container (DESIGN §2.7, §8; issue #22).
//!
//! Every state file is `TNDY`, the container version, the system tag and the blob's length in front
//! of the core's own bytes. These tests pin the layout, the refusal order and the refusal text, so
//! a state of another system can never reach a core it does not belong to.

use tendy_core::state::{HEADER_LEN, MAGIC, VERSION, unwrap, wrap};
use tendy_core::{StateRefusal, SystemKind};

/// A blob of `len` bytes with a pattern and a per-system byte, so a mix-up shows up.
fn blob(tag: u8, len: usize) -> Vec<u8> {
    (0..len)
        .map(|i| (i as u8).wrapping_mul(31).wrapping_add(tag))
        .collect()
}

#[test]
fn wrap_unwrap_round_trip_each_system() {
    for (at, system) in SystemKind::ALL.into_iter().enumerate() {
        let body = blob(at as u8, 257 + at);
        let wrapped = wrap(system, &body);
        assert_eq!(wrapped.len(), HEADER_LEN + body.len());
        assert_eq!(&wrapped[..4], &MAGIC);
        assert_eq!(u16::from_le_bytes([wrapped[4], wrapped[5]]), VERSION);
        assert_eq!(&wrapped[6..10], &system.tag());
        assert_eq!(&wrapped[10..14], &(body.len() as u32).to_le_bytes());
        assert_eq!(
            unwrap(&wrapped, system),
            Ok(&body[..]),
            "{}",
            system.display_name()
        );
        // An empty blob is still a container.
        let empty = wrap(system, &[]);
        assert_eq!(unwrap(&empty, system), Ok(&[][..]));
    }
}

#[test]
fn nes_state_refused_while_gb_runs() {
    let wrapped = wrap(SystemKind::Nes, b"a NES state body");
    let why = unwrap(&wrapped, SystemKind::Gb).expect_err("a NES state is not a Game Boy state");
    assert_eq!(
        why,
        StateRefusal::WrongSystem {
            state: SystemKind::Nes,
            running: SystemKind::Gb
        }
    );
    // The refusal is one plain sentence the drawer can show unchanged.
    let text = why.to_string();
    assert!(text.starts_with("refused: "), "{text}");
    assert!(text.contains("NES save state"), "{text}");
    assert!(text.contains("Game Boy game is running"), "{text}");
}

#[test]
fn legacy_state_of_running_system_accepted() {
    // A state copied from the earlier app starts with the core's own magic and no container: it is
    // handed to the core whole.
    for system in SystemKind::ALL {
        let mut bytes = system.legacy_state_magic().to_vec();
        bytes.extend_from_slice(&blob(0x5A, 64));
        assert_eq!(
            unwrap(&bytes, system),
            Ok(&bytes[..]),
            "{}",
            system.display_name()
        );
    }
}

#[test]
fn legacy_state_of_other_system_refused() {
    for state in SystemKind::ALL {
        for running in SystemKind::ALL {
            if state == running {
                continue;
            }
            let mut bytes = state.legacy_state_magic().to_vec();
            bytes.extend_from_slice(b"legacy body");
            assert_eq!(
                unwrap(&bytes, running),
                Err(StateRefusal::WrongSystem { state, running }),
                "{} state while {} runs",
                state.display_name(),
                running.display_name()
            );
        }
    }
}

#[test]
fn newer_container_refused() {
    let mut wrapped = wrap(SystemKind::Gb, b"body");
    wrapped[4..6].copy_from_slice(&(VERSION + 1).to_le_bytes());
    assert_eq!(
        unwrap(&wrapped, SystemKind::Gb),
        Err(StateRefusal::NewerContainer(VERSION + 1))
    );
    // Only a newer layout is refused: an older container version still reads.
    let mut older = wrap(SystemKind::Gb, b"body");
    older[4..6].copy_from_slice(&0u16.to_le_bytes());
    assert_eq!(unwrap(&older, SystemKind::Gb), Ok(&b"body"[..]));
}

#[test]
fn truncated_container_refused() {
    let wrapped = wrap(SystemKind::Snes, b"a whole SNES body");
    // Everything from the magic to one byte short of the blob is cut short.
    for cut in [4, 6, 10, HEADER_LEN - 1, HEADER_LEN, wrapped.len() - 1] {
        assert_eq!(
            unwrap(&wrapped[..cut], SystemKind::Snes),
            Err(StateRefusal::Truncated),
            "{cut} bytes"
        );
    }
    // A declared length past the end is cut short too.
    let mut lying = wrapped.clone();
    lying[10..14].copy_from_slice(&(wrapped.len() as u32).to_le_bytes());
    assert_eq!(
        unwrap(&lying, SystemKind::Snes),
        Err(StateRefusal::Truncated)
    );
}

#[test]
fn unknown_tag_refused() {
    let mut wrapped = wrap(SystemKind::Gba, b"body");
    wrapped[6..10].copy_from_slice(b"XXXX");
    assert_eq!(
        unwrap(&wrapped, SystemKind::Gba),
        Err(StateRefusal::NotAState)
    );
    // A tag that nearly matches is still not a system tag.
    wrapped[6..10].copy_from_slice(b"GB \0");
    assert_eq!(
        unwrap(&wrapped, SystemKind::Gb),
        Err(StateRefusal::NotAState)
    );
}
