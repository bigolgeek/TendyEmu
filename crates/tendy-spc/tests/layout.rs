//! DESIGN §4.2's state contract, host by host (issue #6).
//!
//! `baselines/pins.toml` settles the exact bytes each host writes (`spc/snes-synthetic`,
//! `spc/sgb-synthetic`); these tests settle the two properties that make the shared writer produce
//! them, and they are the host's alone: how RAM is framed, and how wide `brr` is.
//!
//! Why it matters: a state file is a promise. A SNES save written before the move has to load
//! afterwards, and a Game Boy save with it. If the shared writer picked one host's layout for both,
//! every existing save would either be misread or fail the magic/length check — so each host's
//! layout is asserted here in its own terms, not just as a hash.

use tendy_gb::state::StateWriter as GbWriter;
use tendy_snes::state::StateWriter as SnesWriter;
use tendy_spc::{APU_RAM_SIZE, Apu, Host};

/// A chip with a recognisable RAM pattern and one voice's `brr` above 16 bits.
///
/// The pattern is `i as u8`, so no four consecutive bytes of it can spell the marker below; the
/// marker is `0x1234_5678`, whose halves are distinct enough to tell a two-byte write from a
/// four-byte one.
fn rig(host: Host) -> Apu {
    let mut apu = Apu::new(host);
    for (i, byte) in apu.ram.iter_mut().enumerate() {
        *byte = i as u8;
    }
    apu.dsp.voices[0].brr = 0x1234_5678;
    apu
}

/// The SNES wrote RAM with no framing at all and `brr` as a `u32`; the shared writer still does.
#[test]
fn snes_host_state_matches_snes_layout() {
    let apu = rig(Host::Snes);
    let mut writer = SnesWriter::new();
    apu.save_state(&mut writer);
    let blob = writer.finish();

    assert_eq!(
        &blob[..APU_RAM_SIZE],
        &*apu.ram,
        "RAM is the first field, raw: the SNES wrote no length prefix"
    );
    assert!(
        contains(&blob, &0x1234_5678u32.to_le_bytes()),
        "brr is four bytes wide, as the SNES wrote it"
    );
}

/// The SGB length-prefixes RAM and wrote `brr` as a `u16`; the shared writer still does.
#[test]
fn sgb_host_state_matches_gb_layout() {
    let apu = rig(Host::Sgb);
    let mut writer = GbWriter::new();
    apu.save_state(&mut writer);
    let blob = writer.finish();

    let prefix = (APU_RAM_SIZE as u32).to_le_bytes();
    assert_eq!(
        &blob[..4],
        &prefix,
        "RAM is length-prefixed, as the SGB wrote it"
    );
    assert_eq!(
        &blob[4..4 + APU_RAM_SIZE],
        &*apu.ram,
        "and its bytes follow the prefix"
    );
    assert!(
        !contains(&blob, &0x1234_5678u32.to_le_bytes()),
        "brr is two bytes wide: the top half of the address cannot reach the state"
    );
    assert!(
        contains(&blob, &0x5678u16.to_le_bytes()),
        "but its low half is there"
    );
}

/// Whether `needle` appears anywhere in `haystack`.
fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

/// A chip with something worth saving on every field the state carries.
fn lived_in(host: Host) -> Apu {
    let mut apu = rig(host);
    apu.tick(1_000_000);
    apu.write_port(0, 0x5A);
    apu.write_port(1, 0xA5);
    apu.dsp.write(0x6C, 0x20);
    apu.read_samples(&mut [0.0; 64]);
    apu
}

/// Save → load → save is the same bytes on both hosts, so a file written before the move still
/// loads and a reloaded machine carries on with the same state. The SNES machine's own
/// state is a 38-byte stub that leaves the APU out, so this is the only roundtrip the chip has.
#[test]
fn both_hosts_state_round_trips() {
    let apu = lived_in(Host::Snes);
    let mut writer = SnesWriter::new();
    apu.save_state(&mut writer);
    let blob = writer.finish();
    let mut loaded = Apu::new(Host::Snes);
    let mut reader = tendy_snes::state::StateReader::new(&blob);
    loaded.load_state(&mut reader).expect("the SNES blob loads");
    let mut writer = SnesWriter::new();
    loaded.save_state(&mut writer);
    assert_eq!(
        blob,
        writer.finish(),
        "SNES: save, load, save is the same bytes"
    );

    let apu = lived_in(Host::Sgb);
    let mut writer = GbWriter::new();
    apu.save_state(&mut writer);
    let blob = writer.finish();
    let mut loaded = Apu::new(Host::Sgb);
    let mut reader = tendy_gb::state::StateReader::new(&blob);
    loaded.load_state(&mut reader).expect("the SGB blob loads");
    let mut writer = GbWriter::new();
    loaded.save_state(&mut writer);
    assert_eq!(
        blob,
        writer.finish(),
        "SGB: save, load, save is the same bytes"
    );
}
