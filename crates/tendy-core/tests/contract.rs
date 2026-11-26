//! The tendy-core contract tests (DESIGN §8, #4).

use std::collections::HashSet;

use tendy_core::state::{HEADER_LEN, MAGIC, VERSION, unwrap, wrap};
use tendy_core::{Buttons, Capabilities, Capability, Frame, StateRefusal, SystemKind, fnv1a64};

#[test]
fn system_tags_round_trip() {
    for system in SystemKind::ALL {
        assert_eq!(SystemKind::from_tag(system.tag()), Some(system));
    }
}

#[test]
fn system_keys_unique() {
    let mut seen = HashSet::new();
    for system in SystemKind::ALL {
        assert!(seen.insert(system.key()), "duplicate key {}", system.key());
    }
    assert_eq!(seen.len(), SystemKind::ALL.len());
}

#[test]
fn legacy_magics_unique() {
    let mut seen = HashSet::new();
    for system in SystemKind::ALL {
        assert!(seen.insert(system.legacy_state_magic()));
    }
    assert_eq!(seen.len(), SystemKind::ALL.len());
}

#[test]
fn buttons_bits_distinct() {
    let mut seen = 0u16;
    for (button, name) in Buttons::ALL {
        assert_eq!(button.0.count_ones(), 1, "{name} must be exactly one bit");
        assert_eq!(seen & button.0, 0, "{name} repeats a bit");
        seen |= button.0;
    }
    assert_eq!(seen.count_ones(), Buttons::ALL.len() as u32);
}

#[test]
fn capabilities_with_has() {
    let all = [
        Capability::ChannelMask,
        Capability::Cheats,
        Capability::SgbBorder,
        Capability::SgbSound,
        Capability::BootRom,
        Capability::Palettes,
        Capability::LcdFilter,
        Capability::Link,
        Capability::Tilt,
        Capability::DiskSwap,
        Capability::Debugger,
        Capability::Trace,
        Capability::MemoryView,
        Capability::Movies,
        Capability::Tas,
        Capability::RunAhead,
        Capability::HdPacks,
        Capability::CrtNtsc,
        Capability::Shaders,
        Capability::RetroAchievements,
        Capability::NsfPlayer,
        Capability::VsSystem,
    ];
    assert_eq!(Capabilities::default(), Capabilities::NONE);
    for (i, cap) in all.iter().enumerate() {
        let caps = Capabilities::NONE.with(*cap);
        assert!(caps.has(*cap));
        for (j, other) in all.iter().enumerate() {
            if i != j {
                assert!(!caps.has(*other), "{other:?} must not be set by {cap:?}");
            }
        }
    }
}

#[test]
fn fnv1a64_known_vectors() {
    assert_eq!(fnv1a64(b""), 0xcbf2_9ce4_8422_2325);
    assert_eq!(fnv1a64(b"a"), 0xaf63_dc4c_8601_ec8c);
}

#[test]
fn frame_is_complete_checks_length() {
    let rgba = [0u8; 24];
    assert!(Frame::new(2, 3, &rgba).is_complete());
    assert!(!Frame::new(2, 3, &rgba[..23]).is_complete());
    assert!(!Frame::new(2, 3, &[]).is_complete());
}

#[test]
fn state_refusal_texts_start_refused() {
    let refusals = [
        StateRefusal::NotAState,
        StateRefusal::NewerContainer(2),
        StateRefusal::WrongSystem {
            state: SystemKind::Nes,
            running: SystemKind::Gb,
        },
        StateRefusal::Truncated,
        StateRefusal::Core("checksum mismatch".to_owned()),
    ];
    for refusal in &refusals {
        assert!(
            refusal.to_string().starts_with("refused: "),
            "{refusal:?} prints {:?}",
            refusal.to_string()
        );
    }
}

#[test]
fn state_container_round_trip_each_system() {
    let blob: Vec<u8> = (0..=255).chain(0..=255).collect();
    for system in SystemKind::ALL {
        let wrapped = wrap(system, &blob);
        assert_eq!(wrapped.len(), HEADER_LEN + blob.len());
        assert_eq!(&wrapped[..4], &MAGIC);
        assert_eq!(u16::from_le_bytes([wrapped[4], wrapped[5]]), VERSION);
        assert_eq!(&wrapped[6..10], &system.tag());
        assert_eq!(&wrapped[10..14], &(blob.len() as u32).to_le_bytes());
        assert_eq!(unwrap(&wrapped, system), Ok(&blob[..]));
    }
}

#[test]
fn state_container_refuses_other_system() {
    let wrapped = wrap(SystemKind::Nes, b"NESSTATE");
    assert_eq!(
        unwrap(&wrapped, SystemKind::Gb),
        Err(StateRefusal::WrongSystem {
            state: SystemKind::Nes,
            running: SystemKind::Gb
        })
    );
}

#[test]
fn legacy_state_of_running_system_accepted_whole() {
    let mut bytes = Vec::from(*b"GBST");
    bytes.extend_from_slice(b"legacy body");
    assert_eq!(unwrap(&bytes, SystemKind::Gb), Ok(&bytes[..]));
}

#[test]
fn legacy_state_of_other_system_refused() {
    let bytes = b"GBAG legacy body";
    assert_eq!(
        unwrap(bytes, SystemKind::Gb),
        Err(StateRefusal::WrongSystem {
            state: SystemKind::Gba,
            running: SystemKind::Gb
        })
    );
}

#[test]
fn newer_container_refused() {
    let mut wrapped = wrap(SystemKind::Gb, b"body");
    wrapped[4..6].copy_from_slice(&(VERSION + 1).to_le_bytes());
    assert_eq!(
        unwrap(&wrapped, SystemKind::Gb),
        Err(StateRefusal::NewerContainer(VERSION + 1))
    );
}

#[test]
fn unknown_tag_refused() {
    let mut wrapped = wrap(SystemKind::Gb, b"body");
    wrapped[6..10].copy_from_slice(b"XXXX");
    assert_eq!(
        unwrap(&wrapped, SystemKind::Gb),
        Err(StateRefusal::NotAState)
    );
}

#[test]
fn truncated_container_refused() {
    let wrapped = wrap(SystemKind::Gb, b"body");
    assert_eq!(
        unwrap(&wrapped[..wrapped.len() - 1], SystemKind::Gb),
        Err(StateRefusal::Truncated)
    );
    assert_eq!(
        unwrap(&wrapped[..HEADER_LEN - 4], SystemKind::Gb),
        Err(StateRefusal::Truncated)
    );
}

#[test]
fn not_a_state_refused() {
    assert_eq!(
        unwrap(b"garbage", SystemKind::Gb),
        Err(StateRefusal::NotAState)
    );
}
