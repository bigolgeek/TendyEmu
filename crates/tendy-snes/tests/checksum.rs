//! CRC-32 and hex. The vectors are the published ones.

mod checksum {
    use tendy_snes::checksum::{crc32, crc32_continue, hex};

    /// The published IEEE vectors: the check value from the CRC catalogue, the empty string and the
    /// pangram, whose values are in every write-up of CRC-32/ISO-HDLC.
    #[test]
    fn crc32_known_vectors() {
        assert_eq!(crc32(b""), 0x0000_0000, "the empty input is 0");
        assert_eq!(
            crc32(b"123456789"),
            0xCBF4_3926,
            "the catalogue's check value for CRC-32/ISO-HDLC (poly 0x04C11DB7, reflected)"
        );
        assert_eq!(
            crc32(b"The quick brown fox jumps over the lazy dog"),
            0x414F_A339,
            "the pangram's published CRC-32"
        );
        // Continuing is the same as hashing the whole thing at once.
        let split = crc32_continue(crc32(b"1234"), b"56789");
        assert_eq!(split, 0xCBF4_3926);
        // One changed byte changes the value: the property a `.srm` name depends on.
        assert_ne!(crc32(b"123456789"), crc32(b"123456788"));
    }

    /// Lower case, two digits a byte, so a hex string is fixed-width and case-stable.
    #[test]
    fn hex_is_lowercase_padded() {
        assert_eq!(hex(&[]), "");
        assert_eq!(hex(&[0x00]), "00");
        assert_eq!(hex(&[0x0F]), "0f", "one digit is padded");
        assert_eq!(hex(&[0xAB, 0xCD]), "abcd");
        assert_eq!(hex(&[0xDE, 0xAD, 0xBE, 0xEF]), "deadbeef");
        // Every byte is two characters, and no upper-case letter appears.
        let all: Vec<u8> = (0..=255u8).collect();
        let text = hex(&all);
        assert_eq!(text.len(), 512);
        assert!(
            text.chars()
                .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
        );
    }
}
