//! MD5, CRC32 and base64, by hand: FM2's `romChecksum` is the base64 of the
//! 16-byte MD5 of the ROM data, PNG chunks and BPS patch footers carry CRC32,
//! and this crate has no dependencies to spend on either.

/// Per-round left-rotate counts (RFC 1321 §3.4).
const ROTATE: [u32; 64] = [
    7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9,
    14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10, 15,
    21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
];

/// Per-round additive constants: `floor(2^32 * abs(sin(i + 1)))` (RFC 1321 §3.4).
const SINE: [u32; 64] = [
    0xd76a_a478,
    0xe8c7_b756,
    0x2420_70db,
    0xc1bd_ceee,
    0xf57c_0faf,
    0x4787_c62a,
    0xa830_4613,
    0xfd46_9501,
    0x6980_98d8,
    0x8b44_f7af,
    0xffff_5bb1,
    0x895c_d7be,
    0x6b90_1122,
    0xfd98_7193,
    0xa679_438e,
    0x49b4_0821,
    0xf61e_2562,
    0xc040_b340,
    0x265e_5a51,
    0xe9b6_c7aa,
    0xd62f_105d,
    0x0244_1453,
    0xd8a1_e681,
    0xe7d3_fbc8,
    0x21e1_cde6,
    0xc337_07d6,
    0xf4d5_0d87,
    0x455a_14ed,
    0xa9e3_e905,
    0xfcef_a3f8,
    0x676f_02d9,
    0x8d2a_4c8a,
    0xfffa_3942,
    0x8771_f681,
    0x6d9d_6122,
    0xfde5_380c,
    0xa4be_ea44,
    0x4bde_cfa9,
    0xf6bb_4b60,
    0xbebf_bc70,
    0x289b_7ec6,
    0xeaa1_27fa,
    0xd4ef_3085,
    0x0488_1d05,
    0xd9d4_d039,
    0xe6db_99e5,
    0x1fa2_7cf8,
    0xc4ac_5665,
    0xf429_2244,
    0x432a_ff97,
    0xab94_23a7,
    0xfc93_a039,
    0x655b_59c3,
    0x8f0c_cc92,
    0xffef_f47d,
    0x8584_5dd1,
    0x6fa8_7e4f,
    0xfe2c_e6e0,
    0xa301_4314,
    0x4e08_11a1,
    0xf753_7e82,
    0xbd3a_f235,
    0x2ad7_d2bb,
    0xeb86_d391,
];

/// The RFC 1321 digest, as 16 bytes in the order the hash is printed.
pub fn md5(data: &[u8]) -> [u8; 16] {
    // Pad to 56 bytes mod 64 with a 0x80 byte then zeros, then the bit length
    // as a little-endian u64 (the length counts modulo 2^64, as RFC 1321 says).
    let mut message = Vec::with_capacity(data.len() + 72);
    message.extend_from_slice(data);
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&(data.len() as u64).wrapping_mul(8).to_le_bytes());

    let mut state: [u32; 4] = [0x6745_2301, 0xefcd_ab89, 0x98ba_dcfe, 0x1032_5476];
    for block in message.as_chunks::<64>().0 {
        compress(&mut state, block);
    }

    let mut digest = [0u8; 16];
    for (i, word) in state.iter().enumerate() {
        digest[i * 4..i * 4 + 4].copy_from_slice(&word.to_le_bytes());
    }
    digest
}

/// One 64-byte block of the RFC 1321 compression function, folded into `state`.
fn compress(state: &mut [u32; 4], block: &[u8]) {
    let mut words = [0u32; 16];
    for (i, word) in words.iter_mut().enumerate() {
        let b = &block[i * 4..i * 4 + 4];
        *word = u32::from_le_bytes([b[0], b[1], b[2], b[3]]);
    }

    let [mut a, mut b, mut c, mut d] = *state;
    for i in 0..64 {
        // Four rounds of 16 steps; each round has its own mixing function and
        // message-word order.
        let (f, g) = match i / 16 {
            0 => ((b & c) | (!b & d), i),
            1 => ((d & b) | (!d & c), (5 * i + 1) % 16),
            2 => (b ^ c ^ d, (3 * i + 5) % 16),
            _ => (c ^ (b | !d), (7 * i) % 16),
        };
        let sum = a
            .wrapping_add(f)
            .wrapping_add(SINE[i])
            .wrapping_add(words[g]);
        a = d;
        d = c;
        c = b;
        b = b.wrapping_add(sum.rotate_left(ROTATE[i]));
    }

    state[0] = state[0].wrapping_add(a);
    state[1] = state[1].wrapping_add(b);
    state[2] = state[2].wrapping_add(c);
    state[3] = state[3].wrapping_add(d);
}

/// CRC32 over arbitrary bytes: the reflected 0xEDB88320 polynomial the PNG
/// chunk layout pins down (https://png-spec.org/spec/#R9) and the BPS footer
/// uses, whose check value is crc32("123456789") = 0xCBF43926.
pub fn crc32(bytes: &[u8]) -> u32 {
    crc32_continue(0, bytes)
}

/// CRC32 continued from a previous digest, in the zlib `crc32()` sense:
/// `crc32_continue(crc32(a), b) == crc32(a ++ b)`. The game database's key
/// (`gamedb::rom_crc32`) is the hash of the PRG ROM followed by the CHR ROM,
/// which is this continuation rather than a hash of either half alone.
pub fn crc32_continue(crc: u32, bytes: &[u8]) -> u32 {
    let mut c = !crc;
    for &b in bytes {
        c ^= b as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 {
                (c >> 1) ^ 0xEDB8_8320
            } else {
                c >> 1
            };
        }
    }
    !c
}

/// Lowercase hex, two digits per byte — the form MD5 hashes are usually quoted in.
pub fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(nibble(b >> 4));
        out.push(nibble(b & 0x0F));
    }
    out
}

fn nibble(value: u8) -> char {
    char::from(if value < 10 {
        b'0' + value
    } else {
        b'a' + value - 10
    })
}

/// The RFC 4648 base64 alphabet, in value order.
const BASE64: [u8; 64] = *b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Standard base64 with `=` padding (RFC 4648 §4).
pub fn base64(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0];
        let b1 = chunk.get(1).copied().unwrap_or(0);
        let b2 = chunk.get(2).copied().unwrap_or(0);
        out.push(char::from(BASE64[usize::from(b0 >> 2)]));
        out.push(char::from(
            BASE64[usize::from(((b0 & 0x03) << 4) | (b1 >> 4))],
        ));
        out.push(if chunk.len() > 1 {
            char::from(BASE64[usize::from(((b1 & 0x0F) << 2) | (b2 >> 6))])
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            char::from(BASE64[usize::from(b2 & 0x3F)])
        } else {
            '='
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn md5_known_vectors() {
        assert_eq!(hex(&md5(b"")), "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(hex(&md5(b"abc")), "900150983cd24fb0d6963f7d28e17f72");
        assert_eq!(
            hex(&md5(b"message digest")),
            "f96b697d7cb7938d525a2f31aaf161d0"
        );
        assert_eq!(
            hex(&md5(b"The quick brown fox jumps over the lazy dog")),
            "9e107d9d372bb6826bd81d3542a419d6"
        );
        // Spans many blocks: 1,000,000 'a's is an RFC 1321 test-suite input.
        assert_eq!(
            hex(&md5(&vec![b'a'; 1_000_000])),
            "7707d6ae4e027c70eea2a935c2296f21"
        );
    }

    #[test]
    fn md5_padding_boundaries() {
        // 55, 56, 63 and 64 bytes straddle the padding's corner cases; the
        // lengths just below/above a block boundary must not collide.
        let long = |n: usize| hex(&md5(&vec![b'a'; n]));
        assert_eq!(long(55).len(), 32);
        assert_eq!(long(56).len(), 32);
        assert_ne!(long(55), long(56));
        assert_ne!(long(63), long(64));
    }

    #[test]
    fn crc32_known_vectors() {
        // The check value named in PNG's chunk-layout section
        // (https://png-spec.org/spec/#R9), plus two values cross-checked against
        // CPython's zlib.crc32.
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(b""), 0x0000_0000);
        assert_eq!(
            crc32(b"The quick brown fox jumps over the lazy dog"),
            0x414F_A339
        );
    }

    #[test]
    fn crc32_chains_across_slices() {
        // `crc32_continue(crc32(a), b)` must equal the one-shot hash of a followed by b, which is
        // how the game database hashes PRG then CHR.
        let (a, b) = (b"PRG ROM bytes".as_slice(), b"CHR ROM bytes".as_slice());
        let joined: Vec<u8> = a.iter().chain(b).copied().collect();
        assert_eq!(crc32_continue(crc32(a), b), crc32(&joined));
        // Continuing from nothing is the same as hashing the bytes.
        assert_eq!(crc32_continue(0, a), crc32(a));
        assert_eq!(crc32_continue(crc32(a), b""), crc32(a));
    }

    #[test]
    fn base64_rfc4648_vectors() {
        let cases = [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ];
        for (input, expected) in cases {
            assert_eq!(base64(input.as_bytes()), expected, "base64({input:?})");
        }
    }

    #[test]
    fn base64_pads_a_16_byte_digest_to_24_chars() {
        assert_eq!(base64(&[0u8; 16]), "AAAAAAAAAAAAAAAAAAAAAA==");
        assert_eq!(base64(&[0u8; 16]).len(), 24);
    }
}
