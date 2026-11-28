//! The shared soft-patch tests (DESIGN §8, #30).
//!
//! Every patch is built byte by byte here, so a decoder bug cannot hide behind a rounded trip
//! through the module's own encoder. The one shared helper is the footer CRC32, and the first
//! test pins it to its known vector.

use std::path::PathBuf;

use tendy_detect::patch::{
    PatchError, apply_path, beside, bps_patch, crc32, ips_patch, patch, ups_patch,
};

/// A fresh empty folder per test, named with the process id so parallel tests cannot collide.
fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("tendy_patch_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("the scratch folder is writable");
    dir
}

/// One IPS record: 3-byte offset, 16-bit length, then the data.
fn ips_record(offset: u32, data: &[u8]) -> Vec<u8> {
    let mut out = vec![
        (offset >> 16) as u8,
        (offset >> 8) as u8,
        offset as u8,
        (data.len() >> 8) as u8,
        data.len() as u8,
    ];
    out.extend_from_slice(data);
    out
}

/// One IPS RLE record: 16-bit length 0, 16-bit repeat count, value.
fn ips_rle(offset: u32, count: u16, value: u8) -> Vec<u8> {
    vec![
        (offset >> 16) as u8,
        (offset >> 8) as u8,
        offset as u8,
        0,
        0,
        (count >> 8) as u8,
        count as u8,
        value,
    ]
}

/// An IPS file from already-built records.
fn ips(records: &[Vec<u8>]) -> Vec<u8> {
    let mut patch = b"PATCH".to_vec();
    for record in records {
        patch.extend_from_slice(record);
    }
    patch.extend_from_slice(b"EOF");
    patch
}

/// The BPS/UPS variable-length number, written from the spec's pseudocode.
fn varint(mut data: u64) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let x = (data & 0x7F) as u8;
        data >>= 7;
        if data == 0 {
            out.push(0x80 | x);
            return out;
        }
        out.push(x);
        data -= 1;
    }
}

/// Assemble a BPS file from already-encoded action bytes. The patch CRC is computed over
/// everything before it, so a deliberately wrong action or footer survives to the check it is
/// meant to trip.
fn bps_build(
    source_size: u64,
    target_size: u64,
    actions: &[u8],
    source_crc: u32,
    target_crc: u32,
) -> Vec<u8> {
    let mut patch = b"BPS1".to_vec();
    patch.extend_from_slice(&varint(source_size));
    patch.extend_from_slice(&varint(target_size));
    patch.extend_from_slice(&varint(0)); // no metadata
    patch.extend_from_slice(actions);
    patch.extend_from_slice(&source_crc.to_be_bytes());
    patch.extend_from_slice(&target_crc.to_be_bytes());
    let patch_crc = crc32(&patch);
    patch.extend_from_slice(&patch_crc.to_be_bytes());
    patch
}

/// One UPS block: the relative skip, the XOR bytes, then the `0x00` terminator.
fn ups_record(skip: u64, xor: &[u8]) -> Vec<u8> {
    let mut out = varint(skip);
    out.extend_from_slice(xor);
    out.push(0);
    out
}

/// An UPS file from already-built blocks. The three footer checksums are little-endian.
fn ups(source: &[u8], target: &[u8], records: &[Vec<u8>]) -> Vec<u8> {
    let mut patch = b"UPS1".to_vec();
    patch.extend_from_slice(&varint(source.len() as u64));
    patch.extend_from_slice(&varint(target.len() as u64));
    for record in records {
        patch.extend_from_slice(record);
    }
    patch.extend_from_slice(&crc32(source).to_le_bytes());
    patch.extend_from_slice(&crc32(target).to_le_bytes());
    let patch_crc = crc32(&patch);
    patch.extend_from_slice(&patch_crc.to_le_bytes());
    patch
}

/// Recompute a UPS file's own footer CRC after its body or footer was changed on purpose.
fn resign(patch: &mut [u8]) {
    let at = patch.len() - 4;
    let crc = crc32(&patch[..at]);
    patch[at..].copy_from_slice(&crc.to_le_bytes());
}

#[test]
fn ips_applies() {
    // The known vector for the footer CRC both formats use: crc32("123456789").
    assert_eq!(crc32(b"123456789"), 0xCBF4_3926);

    let rom = b"0123456789";
    let file = ips(&[ips_record(4, b"AB"), ips_record(2, b"YZ")]);
    assert_eq!(patch(rom, &file).unwrap(), b"01YZAB6789");
    // The records apply in file order, so the second one wins where they overlap.
    let overlap = ips(&[ips_record(2, b"XYZ"), ips_record(3, b"Q")]);
    assert_eq!(ips_patch(rom, &overlap).unwrap(), b"01XQZ56789");
}

#[test]
fn ips_rle_record_applies() {
    // A run inside the image and one past its end, which grows the image with zeros first.
    let rom = vec![0u8; 8];
    let file = ips(&[ips_rle(0x0000_0020, 3, 0xFF), ips_rle(1, 4, 0x5A)]);
    let out = ips_patch(&rom, &file).unwrap();
    assert_eq!(out.len(), 0x23);
    assert_eq!(out[0], 0x00);
    assert_eq!(&out[1..5], &[0x5A; 4]);
    assert_eq!(&out[5..8], &[0x00; 3]);
    assert_eq!(&out[8..0x20], &[0x00; 0x18]);
    assert_eq!(&out[0x20..0x23], &[0xFF; 3]);
}

#[test]
fn bps_applies() {
    let source = b"abc";
    let target = b"aXcY";
    // SourceRead 1 (source[0] = 'a'), TargetRead 1 ('X'), SourceRead 1 (source[2] = 'c'),
    // TargetRead 1 ('Y').
    let mut actions = varint(0);
    actions.extend_from_slice(&varint(1));
    actions.push(b'X');
    actions.extend_from_slice(&varint(0));
    actions.extend_from_slice(&varint(1));
    actions.push(b'Y');
    let file = bps_build(
        source.len() as u64,
        target.len() as u64,
        &actions,
        crc32(source),
        crc32(target),
    );
    assert_eq!(bps_patch(source, &file).unwrap(), target);
    assert_eq!(patch(source, &file).unwrap(), target);
}

#[test]
fn bps_bad_crc_refused() {
    let source = b"abc";
    let target = b"aXcY";
    let mut actions = varint(0);
    actions.extend_from_slice(&varint(1));
    actions.push(b'X');
    actions.extend_from_slice(&varint(0));
    actions.extend_from_slice(&varint(1));
    actions.push(b'Y');

    // A patch recorded for a different source revision.
    let wrong_source = bps_build(3, 4, &actions, 0xDEAD_BEEF, crc32(target));
    assert!(matches!(
        bps_patch(source, &wrong_source),
        Err(PatchError::SourceChecksum { .. })
    ));

    // A patch whose own bytes were changed after it was signed.
    let mut corrupted = bps_build(3, 4, &actions, crc32(source), crc32(target));
    corrupted[4] ^= 0x01;
    assert!(matches!(
        bps_patch(source, &corrupted),
        Err(PatchError::PatchChecksum { .. })
    ));

    // A patch that applies but does not produce the image it records.
    let wrong_target = bps_build(3, 4, &actions, crc32(source), 0xDEAD_BEEF);
    assert!(matches!(
        bps_patch(source, &wrong_target),
        Err(PatchError::TargetChecksum { .. })
    ));
}

#[test]
fn ups_applies() {
    // Two changed bytes: output[2] and output[5]. The terminator after the second block lands
    // one past the target, which the format allows - it counts but is not written.
    let source = b"abcdef";
    let target = b"abXdeZ";
    let file = ups(
        source,
        target,
        &[ups_record(2, &[b'c' ^ b'X']), ups_record(1, &[b'f' ^ b'Z'])],
    );
    let out = ups_patch(source, &file);
    assert_eq!(out.unwrap(), target);
    assert_eq!(patch(source, &file).unwrap(), target);

    // A patch that grows the file: the new bytes are XORed against the zero past the input's end.
    let grown = ups(b"abc", b"abcXYZ", &[ups_record(3, b"XYZ")]);
    assert_eq!(ups_patch(b"abc", &grown).unwrap(), b"abcXYZ");
}

#[test]
fn ups_bad_crc_refused() {
    let source = b"abcdef";
    let target = b"abXdeZ";
    let good = ups(
        source,
        target,
        &[ups_record(2, &[b'c' ^ b'X']), ups_record(1, &[b'f' ^ b'Z'])],
    );
    assert_eq!(ups_patch(source, &good).unwrap(), target);

    // A patch recorded for a different input file.
    let mut wrong_source = good.clone();
    let footer = wrong_source.len() - 12;
    wrong_source[footer..footer + 4].copy_from_slice(&0xDEAD_BEEFu32.to_le_bytes());
    resign(&mut wrong_source);
    assert!(matches!(
        ups_patch(source, &wrong_source),
        Err(PatchError::SourceChecksum { .. })
    ));

    // A patch whose own bytes were changed after it was signed.
    let mut corrupted = good.clone();
    corrupted[6] ^= 0x01;
    assert!(matches!(
        ups_patch(source, &corrupted),
        Err(PatchError::PatchChecksum { .. })
    ));

    // A patch that applies but does not produce the image it records.
    let mut wrong_target = good;
    wrong_target[footer + 4..footer + 8].copy_from_slice(&0xDEAD_BEEFu32.to_le_bytes());
    resign(&mut wrong_target);
    assert!(matches!(
        ups_patch(source, &wrong_target),
        Err(PatchError::TargetChecksum { .. })
    ));
}

#[test]
fn patch_beside_rom_found_each_system() {
    let dir = scratch("beside");
    let ips_file = ips(&[ips_record(0, b"Z")]);
    for name in ["cart.gb", "cart.gba", "cart.nes", "cart.sfc"] {
        let rom = dir.join(name);
        std::fs::write(&rom, b"0123456789").expect("the ROM is written");
        std::fs::write(rom.with_extension("ips"), &ips_file).expect("the patch is written");
        let found = beside(&rom).unwrap_or_else(|| panic!(".{name} finds the patch next to it"));
        assert_eq!(found, rom.with_extension("ips"), "{name}");
        let out = apply_path(b"0123456789", &found).expect("the patch applies");
        assert_eq!(out, b"Z123456789", "{name}");
    }

    // Each of the three formats is found by its own extension.
    for extension in ["bps", "ups"] {
        let rom = dir.join(format!("{extension}_rom.bin"));
        std::fs::write(&rom, b"0123456789").expect("the ROM is written");
        std::fs::write(rom.with_extension(extension), b"").expect("the patch is written");
        assert_eq!(beside(&rom), Some(rom.with_extension(extension)));
    }

    // A ROM with no patch beside it has none.
    let bare = dir.join("bare.nes");
    std::fs::write(&bare, b"0123456789").expect("the ROM is written");
    assert_eq!(beside(&bare), None);
    let _ = std::fs::remove_dir_all(&dir);
}
