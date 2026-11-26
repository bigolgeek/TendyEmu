//! The tendy-detect extension fallback tests (DESIGN §8, #13).

use tendy_core::SystemKind;
use tendy_detect::extension::{resolve, system_of};
use tendy_detect::signature::Match;
use tendy_detect::{Basis, Detected, Format, Refusal, detect};

/// Every row of DESIGN §3.3: extension, system, and the format when no signature matched.
const TABLE: &[(&str, SystemKind, Format)] = &[
    ("gb", SystemKind::Gb, Format::Gb),
    ("gbc", SystemKind::Gb, Format::Gb),
    ("sgb", SystemKind::Gb, Format::Gb),
    ("gba", SystemKind::Gba, Format::Gba),
    ("agb", SystemKind::Gba, Format::Gba),
    ("nes", SystemKind::Nes, Format::Ines),
    ("unf", SystemKind::Nes, Format::Unif),
    ("unif", SystemKind::Nes, Format::Unif),
    ("fds", SystemKind::Nes, Format::FdsRaw),
    ("qd", SystemKind::Nes, Format::Qd),
    ("nsf", SystemKind::Nes, Format::Nsf),
    ("sfc", SystemKind::Snes, Format::Snes),
    ("smc", SystemKind::Snes, Format::Snes),
    ("swc", SystemKind::Snes, Format::Snes),
    ("fig", SystemKind::Snes, Format::Snes),
];

/// What `resolve` returns when the extension decides.
fn fallback(system: SystemKind, format: Format) -> Detected {
    Detected {
        system,
        format,
        basis: Basis::Extension,
        notice: None,
        gb: None,
        copier_header: false,
    }
}

/// A signature match with no Game Boy flags and no copier header.
fn sig(system: SystemKind, format: Format) -> Match {
    Match {
        system,
        format,
        gb: None,
        copier_header: false,
    }
}

#[test]
fn every_spec_extension_maps() {
    for &(ext, system, format) in TABLE {
        assert_eq!(
            system_of(ext),
            Some(system),
            ".{ext} names the wrong system"
        );
        let name = format!("game.{ext}");
        assert_eq!(
            resolve(&name, None),
            Ok(fallback(system, format)),
            "{name} should open as {system:?}/{format:?}"
        );
    }
}

#[test]
fn extension_case_insensitive() {
    let cases = [
        ("GAME.GB", SystemKind::Gb, Format::Gb),
        ("homebrew.GbC", SystemKind::Gb, Format::Gb),
        ("border.SgB", SystemKind::Gb, Format::Gb),
        ("disk.QD", SystemKind::Nes, Format::Qd),
        ("music.NsF", SystemKind::Nes, Format::Nsf),
        ("rom.Unif", SystemKind::Nes, Format::Unif),
        ("cart.SFC", SystemKind::Snes, Format::Snes),
        ("cart.Smc", SystemKind::Snes, Format::Snes),
    ];
    for (name, system, format) in cases {
        assert_eq!(resolve(name, None), Ok(fallback(system, format)), "{name}");
        // `system_of` takes the bare extension, in any case.
        let ext = name.rsplit_once('.').unwrap().1;
        assert_eq!(system_of(ext), Some(system), "{name}");
    }
}

#[test]
fn fallback_logo_less_gb() {
    // A decoy header: the flag bytes read as CGB and SGB, but the logo at $0104-$0133 is absent.
    let mut rom = vec![0u8; 0x150];
    rom[0x143] = 0x80;
    rom[0x146] = 0x03;
    rom[0x14B] = 0x33;
    let detected = detect("homebrew.gb", &rom).expect("a .gb file opens by extension");
    assert_eq!(detected.system, SystemKind::Gb);
    assert_eq!(detected.format, Format::Gb);
    assert_eq!(detected.basis, Basis::Extension);
    assert!(detected.notice.is_none());
}

#[test]
fn fallback_raw_qd() {
    // A QD side shares its first block with a raw FDS side (DESIGN §3.6 C5), so a file with no
    // signature opens by extension alone.
    let detected = resolve("disk.qd", None).expect("a .qd file opens by extension");
    assert_eq!(detected.system, SystemKind::Nes);
    assert_eq!(detected.format, Format::Qd);
    assert_eq!(detected.basis, Basis::Extension);
    assert!(detected.notice.is_none());
}

#[test]
fn fds_signature_wins_over_extension() {
    // A fwNES-headered image named `.qd` stays headered: the extension never demotes a signature.
    let detected = resolve("disk.qd", Some(sig(SystemKind::Nes, Format::FdsHeadered)))
        .expect("a headered FDS image opens");
    assert_eq!(detected.format, Format::FdsHeadered);
    assert_eq!(detected.basis, Basis::Signature);
    assert!(detected.notice.is_none());

    // A raw FDS side named `.fds` agrees with its signature.
    let detected =
        resolve("disk.fds", Some(sig(SystemKind::Nes, Format::FdsRaw))).expect("a raw FDS opens");
    assert_eq!(detected.format, Format::FdsRaw);
    assert_eq!(detected.basis, Basis::Signature);
    assert!(detected.notice.is_none());
}

#[test]
fn fds_signature_with_qd_extension_is_qd() {
    // DESIGN §3.3: a raw FDS signature plus `.qd` is a QD image, and the two names agree.
    let detected =
        resolve("disk.qd", Some(sig(SystemKind::Nes, Format::FdsRaw))).expect("a raw QD opens");
    assert_eq!(detected.system, SystemKind::Nes);
    assert_eq!(detected.format, Format::Qd);
    assert_eq!(detected.basis, Basis::Signature);
    assert!(detected.notice.is_none());
}

#[test]
fn conflict_smc_holding_gb_opens_gb_with_notice() {
    let detected =
        resolve("game.smc", Some(sig(SystemKind::Gb, Format::Gb))).expect("the signature decides");
    assert_eq!(detected.system, SystemKind::Gb);
    assert_eq!(detected.format, Format::Gb);
    assert_eq!(detected.basis, Basis::Signature);
    assert_eq!(
        detected.notice.as_deref(),
        Some("file named .smc is a Super NES ROM — opened as Game Boy")
    );

    // The article follows the system the extension names.
    let detected =
        resolve("game.nes", Some(sig(SystemKind::Gb, Format::Gb))).expect("the signature decides");
    assert_eq!(
        detected.notice.as_deref(),
        Some("file named .nes is an NES ROM — opened as Game Boy")
    );
}

#[test]
fn matching_extension_no_notice() {
    assert!(
        resolve("game.gb", Some(sig(SystemKind::Gb, Format::Gb)))
            .unwrap()
            .notice
            .is_none()
    );
    assert!(
        resolve("cart.sfc", Some(sig(SystemKind::Snes, Format::Snes)))
            .unwrap()
            .notice
            .is_none()
    );
    assert!(
        resolve("disk.fds", Some(sig(SystemKind::Nes, Format::FdsHeadered)))
            .unwrap()
            .notice
            .is_none()
    );
}

#[test]
fn unknown_extension_with_signature_no_notice() {
    let detected = resolve("rom.bin", Some(sig(SystemKind::Nes, Format::Ines)))
        .expect("the signature decides");
    assert_eq!(detected.system, SystemKind::Nes);
    assert_eq!(detected.format, Format::Ines);
    assert_eq!(detected.basis, Basis::Signature);
    assert!(detected.notice.is_none());

    // No dot at all: still the signature's file, still no notice.
    let detected =
        resolve("rom", Some(sig(SystemKind::Gba, Format::Gba))).expect("the signature decides");
    assert_eq!(detected.system, SystemKind::Gba);
    assert!(detected.notice.is_none());
}

#[test]
fn no_signature_unknown_extension_refused() {
    for name in [
        "notes.txt",
        "rom",
        "rom.",
        "rom.tar.gz",
        "rom.gb.bak",
        "patch.ips",
    ] {
        let refusal = resolve(name, None).expect_err("no signature and no known extension");
        assert_eq!(
            refusal,
            Refusal::not_a_rom(name),
            "{name} should be refused by name"
        );
    }

    // An empty file has no signature, so it is refused the same way.
    assert_eq!(
        detect("empty.bin", b"").unwrap_err().to_string(),
        "refused: not a recognised ROM (empty.bin)"
    );
}

#[test]
fn truncated_signature_falls_back_to_extension() {
    // Four bytes of an iNES magic are too short for row 1 (DESIGN §3.2), so the extension decides
    // and the NES core refuses the missing header with its own text (§3.3).
    let detected = detect("game.nes", b"NES\x1a").expect("a .nes file opens by extension");
    assert_eq!(detected.system, SystemKind::Nes);
    assert_eq!(detected.format, Format::Ines);
    assert_eq!(detected.basis, Basis::Extension);
    assert!(detected.notice.is_none());
}

// One case per row of the SPEC §3 table (systems in v0.1).

#[test]
fn spec_row_game_boy() {
    for ext in ["gb", "gbc"] {
        let name = format!("game.{ext}");
        assert_eq!(
            resolve(&name, None),
            Ok(fallback(SystemKind::Gb, Format::Gb)),
            "{name}"
        );
    }
}

#[test]
fn spec_row_super_game_boy() {
    assert_eq!(
        resolve("border.sgb", None),
        Ok(fallback(SystemKind::Gb, Format::Gb))
    );
}

#[test]
fn spec_row_game_boy_advance() {
    for ext in ["gba", "agb"] {
        let name = format!("game.{ext}");
        assert_eq!(
            resolve(&name, None),
            Ok(fallback(SystemKind::Gba, Format::Gba)),
            "{name}"
        );
    }
}

#[test]
fn spec_row_nes() {
    for (ext, format) in [
        ("nes", Format::Ines),
        ("unf", Format::Unif),
        ("unif", Format::Unif),
        ("fds", Format::FdsRaw),
        ("qd", Format::Qd),
        ("nsf", Format::Nsf),
    ] {
        let name = format!("game.{ext}");
        assert_eq!(
            resolve(&name, None),
            Ok(fallback(SystemKind::Nes, format)),
            "{name}"
        );
    }
}

#[test]
fn spec_row_super_nes() {
    for ext in ["sfc", "smc", "swc", "fig"] {
        let name = format!("game.{ext}");
        assert_eq!(
            resolve(&name, None),
            Ok(fallback(SystemKind::Snes, Format::Snes)),
            "{name}"
        );
    }
}

#[test]
fn spec_row_archive() {
    // `.zip` and `.7z` are opened before detection (SPEC §4 step 1, #14): they name no system here.
    for ext in ["zip", "7z"] {
        assert_eq!(system_of(ext), None, ".{ext} is not a ROM extension");
        let name = format!("roms.{ext}");
        assert_eq!(resolve(&name, None), Err(Refusal::not_a_rom(&name)));
    }
}
