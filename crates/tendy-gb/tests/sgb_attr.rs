//! `ATTR_TRN`, `ATTR_SET` and the 20×18 two-bit attribute map.
//!
//! Every expectation is Pan Docs' text and nothing else: *ATTR_TRN* gives "90 bytes, that are 5
//! bytes (20×2 bits) for each of the 18 character lines", "the two most significant bits of the
//! first byte define the color attribute (0-3) for the first character of the first line, the next
//! two bits the next character"; 45 files are `$00-$2C`, and "values greater than `$2C` are
//! invalid".
//!
//! The table at the bottom is the leg's whole gate: ten rows, each of which compares all 4 050
//! bytes of the file area **and** all 90 bytes of the live map, so a reader and a writer that agree
//! on the same mistake cannot pass it — the expected maps are either hand-written bytes or built by
//! [`pack`], a writer that ORs two bits per cell rather than shifting the reader's arithmetic back.

mod common;

use tendy_gb::sgb::{ATF_BYTES, ATF_FILES, AttrMap, MaskState, Packet, Sgb};
use tendy_gb::state::{StateReader, StateWriter};

/// The file area's length, spelled out so an off-by-one in the constant cannot hide.
const FILES_BYTES: usize = ATF_FILES * ATF_BYTES;

/// The `$8000-$8FFF` block a transfer reads, all zero.
fn block() -> Vec<u8> {
    vec![0u8; 4096]
}

/// One 4096-byte block with the named bytes set: `(offset, value)`.
fn block_with(bytes: &[(usize, u8)]) -> Vec<u8> {
    let mut v = block();
    for &(offset, value) in bytes {
        v[offset] = value;
    }
    v
}

/// A block whose every byte is a known function of its offset.
fn patterned() -> Vec<u8> {
    (0..4096u32)
        .map(|i| (i.wrapping_mul(7).wrapping_add(3) & 0xFF) as u8)
        .collect()
}

/// The file area as it must look after uploading `block`: its first 4 050 bytes, in order.
fn files_of(block: &[u8]) -> [u8; FILES_BYTES] {
    let mut files = [0u8; FILES_BYTES];
    files.copy_from_slice(&block[..FILES_BYTES]);
    files
}

/// Byte 90 of `block`: what file `n` holds after uploading it.
fn file_of(block: &[u8], n: usize) -> [u8; ATF_BYTES] {
    let mut file = [0u8; ATF_BYTES];
    file.copy_from_slice(&block[n * ATF_BYTES..(n + 1) * ATF_BYTES]);
    file
}

/// A 90-byte map built from `(cell_x, cell_y, palette)` triples — the writer side of the packing.
///
/// Deliberately written as "OR two bits into the byte that holds this cell" from Pan Docs' rule,
/// not as the reader's shift, so a reader that reversed the bit order inside a byte would fail
/// against it.
fn pack(cells: &[(usize, usize, u8)]) -> [u8; ATF_BYTES] {
    let mut map = [0u8; ATF_BYTES];
    for &(x, y, palette) in cells {
        let entry = y * 20 + x;
        assert!(entry < 360, "cell ({x},{y}) is off the 20x18 grid");
        map[entry / 4] |= (palette & 3) << (6 - 2 * (entry % 4));
    }
    map
}

/// Encode a map, decode it into a blank one, and insist the encoding is 4 140 bytes and is read
/// back with nothing left over.
fn round_trip(map: &AttrMap) -> AttrMap {
    let mut w = StateWriter::new();
    map.save_state(&mut w);
    let bytes = w.finish();
    // `StateWriter::bytes` prefixes each run with its length, so the two runs add 8 bytes of
    // framing to the 4 140 of payload.
    assert_eq!(
        bytes.len(),
        FILES_BYTES + ATF_BYTES + 8,
        "4 050 + 90 of payload and two u32 length prefixes"
    );

    let mut r = StateReader::new(&bytes);
    let mut restored = AttrMap::blank();
    restored.load_state(&mut r).expect("the encoding decodes");
    assert_eq!(r.remaining(), 0, "the encoding has no tail");
    restored
}

// ---------------------------------------------------------------- the format, one fact at a time

#[test]
fn a_blank_map_is_all_zero_and_every_cell_is_palette_0() {
    let map = AttrMap::blank();
    assert_eq!(*map.live(), [0u8; ATF_BYTES]);
    assert_eq!(*map.files(), [0u8; FILES_BYTES]);
    for y in 0..18 {
        for x in 0..20 {
            assert_eq!(map.palette_at(x, y), 0, "cell ({x},{y})");
        }
    }
}

#[test]
fn one_byte_holds_four_cells_most_significant_pair_first() {
    // 0xE4 = 11 10 01 00 -> cells 0,1,2,3 of row 0 are palettes 3,2,1,0.
    // 0x1B = 00 01 10 11 -> cells 0,1,2,3 of row 1 are palettes 0,1,2,3 (byte 5).
    let mut map = AttrMap::blank();
    map.attr_trn(&block_with(&[(0, 0xE4), (5, 0x1B)]));
    map.apply_file(0);
    assert_eq!(
        [
            map.palette_at(0, 0),
            map.palette_at(1, 0),
            map.palette_at(2, 0),
            map.palette_at(3, 0)
        ],
        [3, 2, 1, 0],
        "0xE4 = 11 10 01 00"
    );
    assert_eq!(
        [
            map.palette_at(0, 1),
            map.palette_at(1, 1),
            map.palette_at(2, 1),
            map.palette_at(3, 1)
        ],
        [0, 1, 2, 3],
        "0x1B = 00 01 10 11"
    );
    assert_eq!(map.live()[0], 0xE4);
    assert_eq!(map.live()[5], 0x1B);

    // The last cell of the map, entry 359, is byte 89 at bits 1-0.
    let mut last = AttrMap::blank();
    last.attr_trn(&block_with(&[(89, 0x03)]));
    last.apply_file(0);
    assert_eq!(last.palette_at(19, 17), 3);
    assert_eq!(last.palette_at(18, 17), 0, "bits 3-2 of the same byte");
}

#[test]
fn rows_are_twenty_cells_not_eight() {
    // Entry 103 is cell (3,5): 5 * 20 + 3, byte 25, the low pair.
    let mut map = AttrMap::blank();
    map.attr_trn(&block_with(&[(25, 0x03)]));
    map.apply_file(0);
    assert_eq!(map.palette_at(3, 5), 3);
    assert_eq!(map.palette_at(2, 5), 0, "the pair above it, same byte");
    assert_eq!(map.palette_at(4, 5), 0, "the next byte, 26");
    assert_eq!(map.palette_at(3, 4), 0, "the row above, byte 20");
}

#[test]
fn an_upload_fills_the_files_and_leaves_the_screen_alone() {
    let block: Vec<u8> = (0..4096).map(|i| i as u8).collect();
    let mut map = AttrMap::blank();
    map.attr_trn(&block);
    assert_eq!(map.files()[0], 0);
    assert_eq!(map.files()[89], 89);
    assert_eq!(map.files()[90], 90);
    assert_eq!(map.files()[4049], (4049 & 0xFF) as u8);
    assert_eq!(*map.live(), [0u8; ATF_BYTES], "an upload does not show");
}

#[test]
fn attr_set_shows_the_file_it_names() {
    let block: Vec<u8> = (0..4096).map(|i| i as u8).collect();
    let mut map = AttrMap::blank();
    map.attr_trn(&block);
    map.apply_file(1);
    assert_eq!(map.live(), &map.files()[90..180]);
    // 90 = 0b0101_1010 = 01 01 10 10.
    assert_eq!(map.palette_at(0, 0), (90 >> 6) & 3);
    assert_eq!(
        [
            map.palette_at(0, 0),
            map.palette_at(1, 0),
            map.palette_at(2, 0),
            map.palette_at(3, 0)
        ],
        [1, 1, 2, 2]
    );
}

#[test]
fn a_file_number_above_2c_is_ignored() {
    let block = patterned();
    let mut map = AttrMap::blank();
    map.attr_trn(&block);
    map.apply_file(44);
    let file44 = file_of(&block, 44);
    assert_eq!(*map.live(), file44);
    map.apply_file(45);
    assert_eq!(*map.live(), file44, "45 is invalid");
    map.apply_file(63);
    assert_eq!(*map.live(), file44, "63 is invalid");
}

#[test]
fn attr_trn_and_attr_set_through_the_dispatch() {
    let block: Vec<u8> = (0..4096).map(|i| i as u8).collect();
    let mut sgb = Sgb::on();

    // $15 << 3 | 1 = 0xAD, ATTR_TRN. The block is handed in, not in the packet.
    sgb.apply(
        &Packet {
            bytes: [0xAD, 0x55, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
        },
        &block,
    );
    assert_eq!(
        *sgb.attr().live(),
        [0u8; ATF_BYTES],
        "ATTR_TRN shows nothing"
    );

    // $16 << 3 | 1 = 0xB1, ATTR_SET of file 1.
    sgb.apply(
        &Packet {
            bytes: [0xB1, 0x01, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
        },
        &[],
    );
    assert_eq!(sgb.attr().live(), &sgb.attr().files()[90..180]);

    // Bit 6 of ATTR_SET's byte 1 cancels MASK_EN, and the low six bits still name file 1.
    sgb.set_mask(MaskState::Frozen);
    sgb.apply(
        &Packet {
            bytes: [0xB1, 0x41, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
        },
        &[],
    );
    assert_eq!(sgb.mask(), MaskState::Active, "0x41 cancels MASK_EN");
    assert_eq!(sgb.attr().live(), &sgb.attr().files()[90..180]);
}

#[test]
fn save_and_load_restore_the_files_and_the_screen() {
    let block = patterned();
    let mut map = AttrMap::blank();
    map.attr_trn(&block);
    map.apply_file(7);
    assert_eq!(*map.live(), file_of(&block, 7));

    let mut w = StateWriter::new();
    map.save_state(&mut w);
    let bytes = w.finish();
    assert_eq!(bytes.len(), FILES_BYTES + ATF_BYTES + 8);

    let mut r = StateReader::new(&bytes);
    let mut restored = AttrMap::blank();
    restored.load_state(&mut r).expect("the state decodes");
    assert_eq!(restored.files(), map.files());
    assert_eq!(restored.live(), map.live());
    assert_eq!(restored.palette_at(0, 0), map.palette_at(0, 0));
}

/// `PAL_SET`'s ATF bit (`$0A`, flags bit 7) picks one of the uploaded files, and the four system
/// IDs are copied at the same time: one command, two independent effects.
#[test]
fn pal_sets_atf_bit_selects_a_file() {
    let block: Vec<u8> = (0..4096).map(|i| i as u8).collect();
    let mut sgb = Sgb::on();
    sgb.apply(
        &Packet {
            bytes: [0xAD, 0x55, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
        },
        &block,
    );
    // $0A << 3 | 1 = 0x51; four system IDs, then 0x82 = apply the ATF, number 2.
    sgb.apply(
        &Packet {
            bytes: [0x51, 0, 0, 1, 0, 2, 0, 3, 0, 0x82, 0, 0, 0, 0, 0, 0],
        },
        &[],
    );
    assert_eq!(sgb.attr().live(), &sgb.attr().files()[180..270], "ATF2");
}

/// The pixel test of the spec: an ATF cell picks the palette a pixel is drawn with, through
/// `Sgb::colour` and the render hook. The packet vectors are that leg's spec, byte for byte.
#[test]
fn an_atf_cell_picks_the_palette_a_pixel_is_drawn_with() {
    const PAL01: [u8; 16] = [
        0x01, 0x1F, 0x00, 0xE0, 0x03, 0x00, 0x7C, 0xFF, 0x7F, 0x11, 0x11, 0x22, 0x22, 0x33, 0x33,
        0x00,
    ];
    const PAL23: [u8; 16] = [
        0x09, 0xBC, 0x0A, 0x44, 0x44, 0x55, 0x55, 0x66, 0x66, 0x77, 0x77, 0x88, 0x88, 0x99, 0x99,
        0x00,
    ];

    let mut m = common::machine(common::synth_rom(&[]));
    m.bus.ppu.sgb = Sgb::on();
    m.bus.ppu.bgp = 0xE4;
    // Tile 0 and the map are already zero: every pixel is colour index 0.
    m.bus.ppu.sgb.apply(&Packet { bytes: PAL01 }, &[]);
    m.bus.ppu.sgb.apply(&Packet { bytes: PAL23 }, &[]);
    // Cell 0 (pixels 0-7) on palette 3, cell 1 (pixels 8-15) on palette 0: 0xC0 = 11 00 00 00.
    let mut block = block();
    block[0] = 0xC0;
    m.bus.ppu.sgb.apply(
        &Packet {
            bytes: [0xAD, 0x55, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
        },
        &block,
    );
    m.bus.ppu.sgb.apply(
        &Packet {
            bytes: [0xB1, 0x00, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
        },
        &[],
    );
    m.run_frame();

    assert_eq!(m.colours()[0], 0x0ABC, "cell 0 -> palette 3 colour 0");
    assert_eq!(m.colours()[8], 0x001F, "cell 1 -> palette 0 colour 0");
    assert_eq!(m.frame()[0], 0, "the frame buffer keeps its shade meaning");
}

// ---------------------------------------------------------------- the whole map, ten rows

/// One step a row asks [`AttrMap`] to perform, in order.
enum Step {
    /// `ATTR_TRN` with this block.
    Upload(Vec<u8>),
    /// `ATTR_SET` of this file number.
    Select(u8),
    /// A full `save_state` -> `load_state` through a blank map.
    RoundTrip,
}

/// One row of the table: the steps, then the whole 4 050-byte file area and the whole 90-byte map
/// they must leave behind.
struct Row {
    name: &'static str,
    steps: Vec<Step>,
    files: [u8; FILES_BYTES],
    live: [u8; ATF_BYTES],
}

fn rows() -> Vec<Row> {
    let pattern = patterned();
    let other: Vec<u8> = (0..4096u32)
        .map(|i| (i.wrapping_mul(5).wrapping_add(1) & 0xFF) as u8)
        .collect();

    vec![
        // 1. Nothing at all.
        Row {
            name: "blank: no upload, no select",
            steps: vec![],
            files: [0u8; FILES_BYTES],
            live: [0u8; ATF_BYTES],
        },
        // 2. All four cell positions inside one byte, twice over.
        Row {
            name: "one byte = four cells, 0xE4 -> 3,2,1,0 and 0x1B -> 0,1,2,3",
            steps: vec![
                Step::Upload(block_with(&[(0, 0xE4), (5, 0x1B)])),
                Step::Select(0),
            ],
            files: files_of(&block_with(&[(0, 0xE4), (5, 0x1B)])),
            live: pack(&[
                (0, 0, 3),
                (1, 0, 2),
                (2, 0, 1),
                (3, 0, 0),
                (0, 1, 0),
                (1, 1, 1),
                (2, 1, 2),
                (3, 1, 3),
            ]),
        },
        // 3. A cell in each of the four corners of the grid.
        Row {
            name: "four corners: (0,0), (19,0), (0,17), (19,17)",
            steps: vec![
                // entry 0 -> byte 0 bits 7-6; entry 19 -> byte 4 bits 1-0;
                // entry 340 -> byte 85 bits 7-6; entry 359 -> byte 89 bits 1-0.
                Step::Upload(block_with(&[(0, 0x40), (4, 0x03), (85, 0x80), (89, 0x02)])),
                Step::Select(0),
            ],
            files: files_of(&block_with(&[(0, 0x40), (4, 0x03), (85, 0x80), (89, 0x02)])),
            live: pack(&[(0, 0, 1), (19, 0, 3), (0, 17, 2), (19, 17, 2)]),
        },
        // 4. ATTR_TRN copies a known VRAM region and shows nothing.
        Row {
            name: "ATTR_TRN: 4 050 bytes of the $8000-$8FFF block, live untouched",
            steps: vec![Step::Upload(pattern.clone())],
            files: files_of(&pattern),
            live: [0u8; ATF_BYTES],
        },
        // 5. ATTR_SET 44.
        Row {
            name: "ATTR_SET 44: the last valid file",
            steps: vec![Step::Upload(pattern.clone()), Step::Select(44)],
            files: files_of(&pattern),
            live: file_of(&pattern, 44),
        },
        // 6. ATTR_SET 0.
        Row {
            name: "ATTR_SET 0: the first file",
            steps: vec![Step::Upload(pattern.clone()), Step::Select(0)],
            files: files_of(&pattern),
            live: file_of(&pattern, 0),
        },
        // 7. A file number above $2C changes nothing.
        Row {
            name: "ATTR_SET 45 and 63 are invalid and change nothing",
            steps: vec![
                Step::Upload(pattern.clone()),
                Step::Select(44),
                Step::Select(45),
                Step::Select(63),
            ],
            files: files_of(&pattern),
            live: file_of(&pattern, 44),
        },
        // 8. Upload then select shows exactly that file.
        Row {
            name: "ATTR_TRN then ATTR_SET 1",
            steps: vec![Step::Upload(pattern.clone()), Step::Select(1)],
            files: files_of(&pattern),
            live: file_of(&pattern, 1),
        },
        // 9. A second upload replaces the files and leaves the screen on the first one's file.
        Row {
            name: "a second ATTR_TRN replaces the files and not the screen",
            steps: vec![
                Step::Upload(pattern.clone()),
                Step::Select(2),
                Step::Upload(other.clone()),
            ],
            files: files_of(&other),
            live: file_of(&pattern, 2),
        },
        // 10. The state encoding carries both arrays.
        Row {
            name: "save_state then load_state",
            steps: vec![
                Step::Upload(pattern.clone()),
                Step::Select(7),
                Step::RoundTrip,
            ],
            files: files_of(&pattern),
            live: file_of(&pattern, 7),
        },
    ]
}

#[test]
fn every_row_leaves_the_whole_map_and_the_whole_file_area_exactly_as_specified() {
    let rows = rows();
    assert!(
        rows.len() >= 8,
        "the table is the gate: {} rows",
        rows.len()
    );

    for row in rows {
        let mut map = AttrMap::blank();
        for step in &row.steps {
            match step {
                Step::Upload(block) => map.attr_trn(block),
                Step::Select(n) => map.apply_file(*n),
                Step::RoundTrip => map = round_trip(&map),
            }
        }
        assert_eq!(
            *map.files(),
            row.files,
            "{}: the 4 050-byte file area",
            row.name
        );
        assert_eq!(*map.live(), row.live, "{}: the 90-byte live map", row.name);
    }
}
