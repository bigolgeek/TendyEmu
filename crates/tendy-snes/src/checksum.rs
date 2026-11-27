//! CRC-32 and hex, copied from `<gb-rust>`. `fnv1a64` is **not** here: it belongs
//! to `state.rs`.
//!
//! `Cartridge::rom_crc32` is the one caller the whole machine has: it keys a `.srm` and a state
//! file, so the polynomial and the start value are the published IEEE ones and the tests carry the
//! published vectors.

/// Reflected IEEE polynomial.
const POLY: u32 = 0xEDB8_8320;

/// CRC-32 (IEEE) of `bytes`, starting from zero as the published test vectors do.
pub fn crc32(bytes: &[u8]) -> u32 {
    crc32_continue(0, bytes)
}

/// CRC-32 continued from `crc`: the running value as [`crc32`] returned it.
pub fn crc32_continue(crc: u32, bytes: &[u8]) -> u32 {
    let mut state = crc ^ 0xFFFF_FFFF;
    for &b in bytes {
        state ^= u32::from(b);
        for _ in 0..8 {
            let mask = (state & 1).wrapping_neg();
            state = (state >> 1) ^ (POLY & mask);
        }
    }
    state ^ 0xFFFF_FFFF
}

/// Lower-case hex, two digits a byte.
pub fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(char::from_digit((b >> 4) as u32, 16).unwrap_or('0'));
        out.push(char::from_digit((b & 0x0F) as u32, 16).unwrap_or('0'));
    }
    out
}
