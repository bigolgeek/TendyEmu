//! CRC-32 and hex (used by the battery file name and the PNG writer).

mod checksum {
    use tendy_gb::checksum::{crc32, crc32_continue, hex};

    #[test]
    fn crc32_known_vectors() {
        // The published IEEE vectors.
        assert_eq!(crc32(b""), 0x0000_0000);
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(b"a"), 0xE8B7_BE43);
        assert_eq!(crc32(&[0u8; 32]), 0x190A_55AD);
        // Continuing a CRC over split input gives the same value as one call.
        let whole = crc32(b"the quick brown fox");
        let split = crc32_continue(crc32(b"the quick "), b"brown fox");
        assert_eq!(whole, split);
        // A single byte change moves the checksum.
        assert_ne!(crc32(b"Tetris.gb"), crc32(b"Tetris.gc"));
    }

    #[test]
    fn hex_is_lowercase_padded() {
        assert_eq!(hex(&[]), "");
        assert_eq!(hex(&[0x00]), "00");
        assert_eq!(hex(&[0x0F, 0xAB]), "0fab");
        assert_eq!(hex(&[0xA0, 0x00, 0x01]), "a00001");
        assert_eq!(hex(&[0xFF; 4]), "ffffffff");
    }
}
