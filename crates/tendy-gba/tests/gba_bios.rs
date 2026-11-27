//! The BIOS calls and the memory timings, tested where the public suites cannot
//! see.
//!
//! `roms/test/gba/bios.gba` and `memory.gba` are the gates and their tests are at the bottom of this
//! file; everything above them is what those suites cannot show — the exact digits of `Div`, the
//! bytes a decompressor produces from a stream this file constructs, the registers an IRQ handler
//! finds, and the cycles an access costs in each of the Game Pak's three windows.
//!
//! Every claim quotes the document it comes from: GBATEK's *BIOS Functions*, *BIOS Interrupt
//! handling*, *Reading from BIOS Memory*, *4000204h - WAITCNT* and *Address Bus Width and CPU
//! Read/Write Access Widths*, plus the public suite's own sources in `roms/test/gba/src/`.

use tendy_gb::System;
use tendy_gba::Gba;
use tendy_gba::bios::{
    self, Bios, OPCODE_AFTER_BOOT, OPCODE_AFTER_IRQ, OPCODE_AFTER_SWI, OPCODE_DURING_IRQ,
};
use tendy_gba::bus::{
    CYCLES_PER_FRAME, DISPCNT, DISPSTAT, EWRAM_BASE, IE, IME, IO_BASE, IWRAM_BASE, OAM_BASE,
    PALETTE_BASE, ROM_BASE, ROM_WS1_BASE, ROM_WS2_BASE, SRAM_BASE, VBLANK_FIRST_LINE, VRAM_BASE,
    WAITCNT,
};
use tendy_gba::cart::Cartridge;
use tendy_gba::cpu;

/// A cartridge whose code starts at `$080000C0`: the entry point is the usual `b`.
fn synth_cart(code: &[u32]) -> Vec<u8> {
    let mut rom = vec![0u8; 0x200];
    rom[0..4].copy_from_slice(&0xEA00_002Eu32.to_le_bytes());
    rom[0xA0..0xA9].copy_from_slice(b"GBA-BIOS\0");
    rom[0xAC..0xB0].copy_from_slice(b"TBS2");
    rom[0xB0..0xB2].copy_from_slice(b"00");
    rom[0xB2] = 0x96;
    for (index, word) in code.iter().enumerate() {
        let at = 0xC0 + index * 4;
        rom[at..at + 4].copy_from_slice(&word.to_le_bytes());
    }
    let mut x: u8 = 0;
    for &byte in &rom[0xA0..=0xBC] {
        x = x.wrapping_sub(byte);
    }
    rom[0xBD] = x.wrapping_sub(0x19);
    rom
}

/// A machine running `code`, with the cartridge's bytes writable through the bus.
fn machine(code: &[u32]) -> Gba {
    Gba::new(Cartridge::new(synth_cart(code)).expect("a synthesized cartridge"))
}

/// A machine whose cartridge is a plain buffer, for tests that write into ROM first.
fn machine_with_rom(rom: Vec<u8>) -> Gba {
    Gba::new(Cartridge::new(rom).expect("a synthesized cartridge"))
}

/// Call a BIOS function the way an `SWI` does, and report the four argument registers afterwards.
fn call(gba: &mut Gba, number: u32, args: [u32; 4]) -> [u32; 4] {
    gba.cpu.r[0] = args[0];
    gba.cpu.r[1] = args[1];
    gba.cpu.r[2] = args[2];
    gba.cpu.r[3] = args[3];
    bios::call(&mut gba.cpu, &mut gba.bus, number);
    [gba.cpu.r[0], gba.cpu.r[1], gba.cpu.r[2], gba.cpu.r[3]]
}

/// Put `bytes` at `addr` through the bus.
fn put(gba: &mut Gba, addr: u32, bytes: &[u8]) {
    for (index, &byte) in bytes.iter().enumerate() {
        gba.bus.write8(addr + index as u32, byte);
    }
}

/// Read `len` bytes from `addr`.
fn get(gba: &mut Gba, addr: u32, len: usize) -> Vec<u8> {
    (0..len)
        .map(|index| gba.bus.read8(addr + index as u32))
        .collect()
}

/// A compression stream's header: "Bit 0-3 size, Bit 4-7 type, Bit 8-31 24bit size".
fn header(len: u32, kind: u32) -> [u8; 4] {
    ((len << 8) | (kind << 4)).to_le_bytes()
}

// ---------------------------------------------------------------- arithmetic

#[test]
fn div_reports_the_quotient_the_remainder_and_its_magnitude() {
    let mut gba = machine(&[]);
    // GBATEK's own example: "incoming -1234, 10 should return -123, -4, +123".
    let [quotient, remainder, _, magnitude] = call(&mut gba, 0x06, [(-1234i32) as u32, 10, 0, 0]);
    assert_eq!(quotient as i32, -123);
    assert_eq!(remainder as i32, -4);
    assert_eq!(magnitude, 123);

    // The remainder takes the sign of the numerator, as a truncating division's does.
    for (numerator, denominator, want) in [
        (1234i32, 10i32, (123, 4)),
        (-1234, -10, (123, -4)),
        (1234, -10, (-123, 4)),
        (7, 2, (3, 1)),
        (-7, 2, (-3, -1)),
        (0, 5, (0, 0)),
    ] {
        let [quotient, remainder, _, _] =
            call(&mut gba, 0x06, [numerator as u32, denominator as u32, 0, 0]);
        assert_eq!(
            (quotient as i32, remainder as i32),
            want,
            "{numerator} / {denominator}"
        );
    }
}

#[test]
fn div_by_zero_and_the_one_overflow_answer_without_panicking() {
    let mut gba = machine(&[]);
    // GBATEK: division by zero "usually gets caught in an endless loop". A call cannot loop here, so
    // the answer is the ARM convention: quotient zero, the numerator as the remainder.
    let [quotient, remainder, _, magnitude] = call(&mut gba, 0x06, [1234, 0, 0, 0]);
    assert_eq!((quotient, remainder, magnitude), (0, 1234, 0));

    // `i32::MIN / -1` is the one overflow a 32-bit divide has; the hardware's shift-and-subtract
    // loop answers with `i32::MIN` and no remainder.
    let [quotient, remainder, _, magnitude] =
        call(&mut gba, 0x06, [i32::MIN as u32, (-1i32) as u32, 0, 0]);
    assert_eq!(quotient, i32::MIN as u32);
    assert_eq!(remainder, 0);
    assert_eq!(
        magnitude,
        i32::MIN as u32,
        "ABS(i32::MIN) has no larger value"
    );
}

#[test]
fn div_arm_swaps_its_arguments() {
    let mut gba = machine(&[]);
    // "Same as above (SWI 06h Div), but incoming parameters are exchanged, r1/r0 (r0=Denom,
    // r1=number)."
    let [quotient, remainder, _, _] = call(&mut gba, 0x07, [10, (-1234i32) as u32, 0, 0]);
    assert_eq!(quotient as i32, -123);
    assert_eq!(remainder as i32, -4);
}

#[test]
fn sqrt_is_the_integer_root_over_the_whole_unsigned_range() {
    let mut gba = machine(&[]);
    for (value, want) in [
        (0u32, 0u32),
        (1, 1),
        (2, 1),
        (3, 1),
        (4, 2),
        (99, 9),
        (100, 10),
        (65535, 255),
        (65536, 256),
        (u32::MAX, 65535),
    ] {
        let [root, _, _, _] = call(&mut gba, 0x08, [value, 0, 0, 0]);
        assert_eq!(root, want, "Sqrt({value})");
    }
}

#[test]
fn the_angles_are_in_sixteenths_of_a_turn() {
    let mut gba = machine(&[]);
    // ArcTan: 1.14 fixed point in, 1/65536ths of a turn out, 0x4000 = a quarter turn.
    let [angle, _, _, _] = call(&mut gba, 0x09, [0, 0, 0, 0]);
    assert_eq!(angle, 0, "atan(0)");
    let [angle, _, _, _] = call(&mut gba, 0x09, [0x4000, 0, 0, 0]);
    assert_eq!(angle, 0x2000, "atan(1) is an eighth of a turn");
    let [angle, _, _, _] = call(&mut gba, 0x09, [(-0x4000i32) as u32, 0, 0, 0]);
    assert_eq!(angle, 0xE000, "and the negative one is three eighths");

    // ArcTan2: the quadrant comes from the two signs.
    for ((x, y), want) in [
        ((0x4000i32, 0x0000i32), 0x0000u32),
        ((0x0000, 0x4000), 0x4000),
        ((-0x4000, 0x0000), 0x8000),
        ((0x0000, -0x4000), 0xC000),
        ((0x4000, 0x4000), 0x2000),
    ] {
        let [angle, _, _, _] = call(&mut gba, 0x0A, [x as u32, y as u32, 0, 0]);
        assert_eq!(angle, want, "ArcTan2({x}, {y})");
    }
}

// ---------------------------------------------------------------- rotation and scaling

#[test]
fn an_affine_set_with_no_rotation_and_no_scaling_is_the_identity() {
    let mut gba = machine(&[]);
    // The source field: centre (0x800000, 0x800000), display centre (120, 80), scale 1.0 in 8.8,
    // angle 0.
    let source = IWRAM_BASE + 0x400;
    let dest = IWRAM_BASE + 0x500;
    let field: [u32; 7] = [
        0x0080_0000,
        0x0080_0000,
        120 | (80 << 16),
        256 | (256 << 16),
        0,
        0,
        0,
    ];
    for (index, word) in field.iter().enumerate() {
        gba.bus.write32(source + index as u32 * 4, *word);
    }
    call(&mut gba, 0x0E, [source, dest, 1, 0]);
    let read = |gba: &mut Gba, at: u32| -> i32 { gba.bus.read16(at) as i16 as i32 };
    assert_eq!(read(&mut gba, dest), 256, "pa: the X scale, unchanged");
    assert_eq!(read(&mut gba, dest + 2), 0, "pb: no shear");
    assert_eq!(read(&mut gba, dest + 4), 0, "pc: no shear");
    assert_eq!(read(&mut gba, dest + 6), 256, "pd: the Y scale, unchanged");
    // The start pixel is the centre less the display centre, which is what makes the centre of the
    // screen show the centre of the source.
    assert_eq!(
        gba.bus.read32(dest + 8) as i32,
        0x0080_0000 - 120,
        "start X"
    );
    assert_eq!(
        gba.bus.read32(dest + 12) as i32,
        0x0080_0000 - 80,
        "start Y"
    );
}

#[test]
fn a_quarter_turn_swaps_the_axes() {
    let mut gba = machine(&[]);
    let source = IWRAM_BASE + 0x400;
    let dest = IWRAM_BASE + 0x500;
    // Scale 1.0, angle 0x4000 = 90 degrees: sin = 1.0, cos = 0.
    let field: [u32; 7] = [0, 0, 0, 256 | (256 << 16), 0x4000, 0, 0];
    for (index, word) in field.iter().enumerate() {
        gba.bus.write32(source + index as u32 * 4, *word);
    }
    call(&mut gba, 0x0E, [source, dest, 1, 0]);
    let read = |gba: &mut Gba, at: u32| -> i32 { gba.bus.read16(at) as i16 as i32 };
    assert_eq!(read(&mut gba, dest), 0, "pa = scale * cos(90) = 0");
    assert_eq!(read(&mut gba, dest + 2), -256, "pb = -scale * sin(90)");
    assert_eq!(read(&mut gba, dest + 4), 256, "pc = scale * sin(90)");
    assert_eq!(read(&mut gba, dest + 6), 0, "pd = scale * cos(90) = 0");
}

#[test]
fn obj_affine_set_writes_four_parameters_at_the_offset_it_is_given() {
    let mut gba = machine(&[]);
    let source = IWRAM_BASE + 0x400;
    let dest = IWRAM_BASE + 0x500;
    // Two entries, scale 1.0, no rotation, written 8 bytes apart as an OAM affine block is. Each
    // source entry is "s16 scale X, s16 scale Y, u16 angle".
    for entry in 0..2u32 {
        let at = source + entry * 6;
        gba.bus.write16(at, 256);
        gba.bus.write16(at + 2, 256);
        gba.bus.write16(at + 4, 0);
    }
    call(&mut gba, 0x0F, [source, dest, 2, 8]);
    for entry in 0..2 {
        // "The four affine parameters are set every Offset bytes": with an offset of eight they are
        // pa, pb, pc, pd at +0, +8, +16 and +24.
        let at = dest + entry * 32;
        assert_eq!(gba.bus.read16(at), 256, "pa of entry {entry}");
        assert_eq!(gba.bus.read16(at + 8), 0, "pb");
        assert_eq!(gba.bus.read16(at + 16), 0, "pc");
        assert_eq!(gba.bus.read16(at + 24), 256, "pd");
    }
}

// ---------------------------------------------------------------- memory copy

#[test]
fn cpu_set_copies_and_fills_in_halfwords_and_words() {
    let mut gba = machine(&[]);
    for index in 0..8u32 {
        gba.bus.write32(EWRAM_BASE + index * 4, 0x1122_3344 + index);
    }
    // 32-bit copy of eight words: r2 = count | bit26.
    call(&mut gba, 0x0B, [EWRAM_BASE, IWRAM_BASE, 8 | (1 << 26), 0]);
    for index in 0..8u32 {
        assert_eq!(
            gba.bus.read32(IWRAM_BASE + index * 4),
            0x1122_3344 + index,
            "word {index}"
        );
    }
    // A 32-bit fill of four words with bit 24: the source stays put.
    gba.bus.write32(EWRAM_BASE, 0xDEAD_BEEF);
    call(
        &mut gba,
        0x0B,
        [EWRAM_BASE, IWRAM_BASE + 0x100, 4 | (1 << 26) | (1 << 24), 0],
    );
    for index in 0..4u32 {
        assert_eq!(gba.bus.read32(IWRAM_BASE + 0x100 + index * 4), 0xDEAD_BEEF);
    }
    // Sixteen-bit, the default: half of a word's worth.
    call(&mut gba, 0x0B, [EWRAM_BASE, IWRAM_BASE + 0x200, 4, 0]);
    assert_eq!(
        gba.bus.read16(IWRAM_BASE + 0x200),
        0xBEEF,
        "the fill's word"
    );
    assert_eq!(gba.bus.read16(IWRAM_BASE + 0x202), 0xDEAD);
}

#[test]
fn cpu_fast_set_rounds_its_count_up_to_a_whole_block() {
    let mut gba = machine(&[]);
    for index in 0..16u32 {
        gba.bus.write32(EWRAM_BASE + index * 4, index);
    }
    // "GBA: rounded-up to multiple of 8 words (32 bytes)": three words becomes eight.
    call(&mut gba, 0x0C, [EWRAM_BASE, IWRAM_BASE, 3, 0]);
    for index in 0..8u32 {
        assert_eq!(
            gba.bus.read32(IWRAM_BASE + index * 4),
            index,
            "word {index}"
        );
    }
}

#[test]
fn both_copy_calls_refuse_a_source_inside_the_bios() {
    let mut gba = machine(&[]);
    gba.bus.write32(IWRAM_BASE, 0x5555_5555);
    // "On GBA… these two functions will silently reject to do anything if the source start or end
    // addresses are reaching into the BIOS area."
    call(&mut gba, 0x0B, [0x1000, IWRAM_BASE, 8 | (1 << 26), 0]);
    assert_eq!(
        gba.bus.read32(IWRAM_BASE),
        0x5555_5555,
        "nothing was copied"
    );
    call(&mut gba, 0x0C, [0x1000, IWRAM_BASE, 8, 0]);
    assert_eq!(
        gba.bus.read32(IWRAM_BASE),
        0x5555_5555,
        "and nothing filled"
    );
}

// ---------------------------------------------------------------- decompression

#[test]
fn lz77_expands_literals_and_back_references_byte_for_byte() {
    let mut gba = machine(&[]);
    let source = EWRAM_BASE;
    let dest = IWRAM_BASE + 0x200;
    // Twelve bytes: eight literals "ABCDABCD", then a back-reference of four bytes from eight back.
    let stream: Vec<u8> = [
        &header(12, 1)[..],
        &[0b0000_0000][..], // eight literal blocks
        b"A",
        b"B",
        b"C",
        b"D",
        b"A",
        b"B",
        b"C",
        b"D",
        &[0b1000_0000][..], // a compressed block
        &[0x10, 7][..],     // count nibble 1 (four bytes), displacement 7 — which is dest-8
    ]
    .concat();
    put(&mut gba, source, &stream);
    call(&mut gba, 0x11, [source, dest, 0, 0]);
    assert_eq!(get(&mut gba, dest, 12), b"ABCDABCDABCD");
}

#[test]
fn lz77_copies_a_run_that_reads_what_it_is_writing() {
    let mut gba = machine(&[]);
    let source = EWRAM_BASE;
    let dest = IWRAM_BASE + 0x200;
    // Eight 'A's: one literal and two three-byte copies from displacement zero, which is the byte
    // just written — the case GBATEK warns about.
    let stream: Vec<u8> = [
        &header(8, 1)[..],
        &[0b0111_0000][..], // one literal block, then two compressed ones
        b"A",
        &[0x00, 0x00][..],
        &[0x00, 0x00][..],
    ]
    .concat();
    put(&mut gba, source, &stream);
    call(&mut gba, 0x11, [source, dest, 0, 0]);
    assert_eq!(get(&mut gba, dest, 8), b"AAAAAAAA");
}

#[test]
fn the_halfword_variant_reads_back_from_the_byte_the_destination_names() {
    let mut gba = machine(&[]);
    let source = EWRAM_BASE;
    let dest = VRAM_BASE + 0x200;
    // GBATEK's *Block Type 1 - Compressed*: "Copy N+3 Bytes from Dest-Disp-1 to Dest", with `Dest`
    // the address the next byte occupies. Twelve bytes: the four literals "abcd", then one
    // compressed block that copies eight bytes from four back. Eight against a `disp` of three
    // means the second half of the run reads bytes the run itself has just written, so the copy
    // has to see its own output — the case where a halfword sink's pending byte is easy to miss.
    let stream: Vec<u8> = [
        &header(12, 1)[..], // twelve decompressed bytes, type 1 (LZ77)
        &[0b0000_1000][..], // four literal blocks, then one compressed block
        b"abcd",            // the four literals, at Dest 0..4
        &[0x50, 0x03][..],  // count nibble 5 (eight bytes), disp 3 — each byte from Dest-4
    ]
    .concat();
    put(&mut gba, source, &stream);

    // The eight-bit writer has no pending byte and is the reference: `Dest - disp - 1` is
    // `out.len() - disp - 1` there.
    call(&mut gba, 0x11, [source, IWRAM_BASE + 0x200, 0, 0]);
    assert_eq!(
        get(&mut gba, IWRAM_BASE + 0x200, 12),
        b"abcdabcdabcd",
        "the 8-bit sink"
    );

    // The same stream through the halfword writer must produce the same twelve bytes. While a byte
    // waits for its halfword partner it is not in memory yet, but `Dest` has moved past it.
    call(&mut gba, 0x12, [source, dest, 0, 0]);
    assert_eq!(
        get(&mut gba, dest, 12),
        b"abcdabcdabcd",
        "and the 16-bit sink, whose run reads what it is still writing"
    );
}

#[test]
fn the_vram_variant_writes_halfwords_and_puts_an_odd_tail_byte_in_the_low_half() {
    let mut gba = machine(&[]);
    let source = EWRAM_BASE;
    let dest = VRAM_BASE + 0x100;
    // Five literals: "HELLO", an odd length, into VRAM's halfword-only write path.
    let stream: Vec<u8> = [&header(5, 1)[..], &[0b0000_0000][..], b"HELLO"].concat();
    put(&mut gba, source, &stream);
    call(&mut gba, 0x12, [source, dest, 0, 0]);
    assert_eq!(get(&mut gba, dest, 6), b"HELLO\0");
    // The tail byte lands in the low half of the halfword at its own (halfword-aligned) address, so
    // the final 16-bit unit reads $004F: 'O' in the low half and the untouched high half above it.
    assert_eq!(gba.bus.read16(dest + 2), 0x4C4C, "the last whole halfword");
    assert_eq!(
        gba.bus.read16(dest + 4),
        0x004F,
        "and the odd tail byte, as a low half"
    );
}

#[test]
fn run_length_expands_runs_and_literals() {
    let mut gba = machine(&[]);
    let source = EWRAM_BASE;
    let dest = IWRAM_BASE + 0x200;
    // Five bytes: four 'A's from a run, then one literal 'B'. "compressed N-3, uncompressed N-1".
    let stream: Vec<u8> = [&header(5, 3)[..], &[0x80 | 1][..], b"A", &[0x00][..], b"B"].concat();
    put(&mut gba, source, &stream);
    call(&mut gba, 0x14, [source, dest, 0, 0]);
    assert_eq!(get(&mut gba, dest, 5), b"AAAAB");
}

#[test]
fn the_run_length_vram_variant_writes_halfwords_too() {
    let mut gba = machine(&[]);
    let source = EWRAM_BASE;
    let dest = VRAM_BASE + 0x200;
    let stream: Vec<u8> = [&header(4, 3)[..], &[0x80 | 1][..], b"Z"].concat();
    put(&mut gba, source, &stream);
    call(&mut gba, 0x15, [source, dest, 0, 0]);
    assert_eq!(get(&mut gba, dest, 4), b"ZZZZ");
    assert_eq!(
        gba.bus.read16(dest),
        0x5A5A,
        "one halfword write of the same byte"
    );
}

#[test]
fn huffman_walks_the_tree_the_documentation_describes() {
    let mut gba = machine(&[]);
    let source = EWRAM_BASE;
    let dest = IWRAM_BASE + 0x200;
    // The tree GBATEK uses as its example: root.0 is data 'f', root.1 is a node whose own nodes are
    // 'H' and 'u'. The bitstream "10 11 0 0" is "Huff", which is the example's own string.
    //
    // A node's children live at `(address AND NOT 1) + offset*2 + 2` and the byte after that, so the
    // table is built relative to the *even* address below the root. The root sits at an odd address
    // (four header bytes and the size byte before it), which is why the first two children are the
    // bytes after it.
    //
    // The stream is laid out the way the document describes it. `Tree Size (8bit)` is "Bit0-7 Size of
    // Tree Table/2-1 (ie. Offset to Compressed Bitstream)", so the field is the offset in halfwords
    // from *its own byte* to the bitstream: a two means six bytes — the size byte and the five tree
    // bytes below — and the bitstream starts at source + 4 + 2*2 = source + 10, immediately after
    // 'u'. Counting from the root node instead would put it one byte further on, at source + 11,
    // and the same bitstream would then decode as four 'f's.
    let tree: [u8; 5] = [0x80, b'f', 0xC0, b'H', b'u'];
    let bits: u32 = 0b1011_0000 << 24;
    let mut stream = Vec::new();
    stream.extend_from_slice(&header(4, 2));
    stream.push(2); // "Size of Tree Table/2-1": the size byte and five tree bytes are six bytes.
    stream.extend_from_slice(&tree);
    stream.extend_from_slice(&bits.to_le_bytes());
    put(&mut gba, source, &stream);
    call(&mut gba, 0x13, [source, dest, 0, 0]);
    assert_eq!(get(&mut gba, dest, 4), b"Huff");
}

#[test]
fn the_huffman_bitstream_starts_where_the_tree_size_says_and_decodes_a_hand_checked_stream() {
    let mut gba = machine(&[]);
    let source = EWRAM_BASE;
    let dest = IWRAM_BASE + 0x200;
    // A tree of my own, whose every byte is used, and a stream whose output is checked below on
    // paper rather than against the decoder. Nodes are placed by the document's own formula,
    // `(address AND NOT 1) + offset*2 + 2`, from a root at source + 5 (the address after the four
    // header bytes and the size byte):
    //
    //   source+5  root  $40  offset 0, node0 not data, node1 data  -> children source+6, source+7
    //   source+6  node A $80 offset 0, node0 data, node1 not data  -> children source+8, source+9
    //   source+7  data  'o'  (root's node1)
    //   source+8  data  'a'  (A's node0)
    //   source+9  node B $C0 offset 0, both children data          -> children source+10, source+11
    //   source+10 data  'c'  (B's node0)
    //   source+11 data  't'  (B's node1)
    //
    // The codes that follow from it are: 'o' = 1, 'a' = 00, 'c' = 010, 't' = 011.
    //
    // Seven tree bytes make the size field "Size of Tree Table/2-1" = 3, and the document's offset
    // is three halfwords from that byte: the bitstream begins at source + 4 + 3*2 = source + 12,
    // which is also the 4-byte boundary the same page asks for ("Compressed Bitstream (stored in
    // units of 32bits)").
    let tree: [u8; 7] = [0x40, 0x80, b'o', b'a', 0xC0, b'c', b't'];
    // "catcoat" = 010 00 011 010 1 00 011, seventeen bits from Bit31 down:
    //   0100_0011_0101_0001_1 followed by fifteen zeros = $4351_8000.
    let bits: u32 = 0x4351_8000;
    let mut stream = Vec::new();
    stream.extend_from_slice(&header(7, 2));
    stream.push(3);
    stream.extend_from_slice(&tree);
    stream.extend_from_slice(&bits.to_le_bytes());
    assert_eq!(
        stream.len(),
        12 + 4,
        "the bitstream begins on the last four bytes"
    );
    put(&mut gba, source, &stream);
    call(&mut gba, 0x13, [source, dest, 0, 0]);
    assert_eq!(get(&mut gba, dest, 7), b"catcoat");
}

/// A Huffman header whose `Bit0-3` is the data element's width in bits: "normally 4 or 8", with
/// `Bit4-7 Compressed type (must be 2 for Huffman)` and `Bit8-31 24bit size of decompressed data in
/// bytes`.
fn huffman_header(len: u32, unit: u32) -> [u8; 4] {
    ((len << 8) | (2 << 4) | unit).to_le_bytes()
}

#[test]
fn four_bit_huffman_symbols_share_an_output_byte_the_first_in_the_low_half() {
    let mut gba = machine(&[]);
    let source = EWRAM_BASE;
    let dest = IWRAM_BASE + 0x200;
    // A tree of my own, two data leaves and nothing else, checked on paper below:
    //
    //   source+5  root  $C0  offset 0, node0 data, node1 data -> children source+6, source+7
    //   source+6  data  $03  (node0)
    //   source+7  data  $0A  (node1)
    //
    // so the code 0 is the nibble 3 and the code 1 is the nibble A.
    //
    // Three tree bytes give "Size of Tree Table/2-1" = (3+1)/2 - 1 = 1, and the document's offset
    // is one halfword from that byte: the bitstream starts at source + 4 + 1*2 = source + 8, also
    // 4-byte aligned. The header asks for 3 bytes; at four bits an element that is six symbols.
    let tree: [u8; 3] = [0xC0, 0x03, 0x0A];
    // Symbols 3, A, A, 3, 3, A are the codes 0, 1, 1, 0, 0, 1 — `011001` from Bit31 down, then pad.
    let bits: u32 = 0x6400_0000;
    // On paper, one output byte per pair, the first symbol in the low half:
    //   3 | A<<4 = $A3      A | 3<<4 = $3A      3 | A<<4 = $A3
    // Packed the other way round the same six symbols would be $3A $A3 $3A, so the three bytes
    // below are the packing order and not just "some bytes".
    let mut stream = Vec::new();
    stream.extend_from_slice(&huffman_header(3, 4));
    stream.push(1);
    stream.extend_from_slice(&tree);
    stream.extend_from_slice(&bits.to_le_bytes());
    assert_eq!(
        stream.len(),
        12,
        "header, size byte, three tree bytes and one bitstream word"
    );
    put(&mut gba, source, &stream);
    call(&mut gba, 0x13, [source, dest, 0, 0]);
    assert_eq!(
        get(&mut gba, dest, 3),
        [0xA3, 0x3A, 0xA3],
        "two symbols to the byte, the first in the low half"
    );
}

#[test]
fn a_four_bit_stream_stops_at_the_byte_count_and_writes_no_trailing_nibble() {
    let mut gba = machine(&[]);
    let source = EWRAM_BASE;
    let dest = IWRAM_BASE + 0x200;
    // GBATEK: "Bit8-31 24bit size of decompressed data in bytes". A four-bit call is therefore
    // asked for twice as many symbols as it is asked for bytes, and its last byte is always full —
    // there is no odd symbol to flush. The bitstream below is the one above with a seventh symbol
    // (a zero bit, another 3) in it: the header asks for three bytes, the decoder stops after the
    // sixth symbol, and the seventh is left unread rather than written past the caller's length.
    let tree: [u8; 3] = [0xC0, 0x03, 0x0A];
    let bits: u32 = 0b0110_0100_0000_0000 << 16; // `0110010` from Bit31 down
    let mut stream = Vec::new();
    stream.extend_from_slice(&huffman_header(3, 4));
    stream.push(1);
    stream.extend_from_slice(&tree);
    stream.extend_from_slice(&bits.to_le_bytes());
    put(&mut gba, source, &stream);
    put(&mut gba, dest, &[0xEE; 8]);
    call(&mut gba, 0x13, [source, dest, 0, 0]);
    assert_eq!(get(&mut gba, dest, 3), [0xA3, 0x3A, 0xA3]);
    assert_eq!(
        get(&mut gba, dest + 3, 1),
        [0xEE],
        "nothing past the three bytes the header asked for"
    );
}

#[test]
fn the_difference_filters_add_each_unit_to_the_one_before_it() {
    let mut gba = machine(&[]);
    let source = EWRAM_BASE;
    let dest = IWRAM_BASE + 0x200;
    // GBATEK's own example: unfiltered 10..13 is filtered 10, +1, +1, +1.
    let stream: Vec<u8> = [&header(4, 8)[..], &[10, 1, 1, 1]].concat();
    put(&mut gba, source, &stream);
    call(&mut gba, 0x16, [source, dest, 0, 0]);
    assert_eq!(get(&mut gba, dest, 4), [10, 11, 12, 13]);

    // The same through the halfword-writing variant, into VRAM.
    let stream: Vec<u8> = [&header(4, 8)[..], &[10, 1, 1, 1]].concat();
    put(&mut gba, source, &stream);
    call(&mut gba, 0x17, [source, VRAM_BASE + 0x300, 0, 0]);
    assert_eq!(get(&mut gba, VRAM_BASE + 0x300, 4), [10, 11, 12, 13]);

    // And sixteen-bit units: 0x0100 then a delta of three.
    let stream: Vec<u8> = [&header(4, 8)[..], &[0x00, 0x01, 0x03, 0x00]].concat();
    put(&mut gba, source, &stream);
    call(&mut gba, 0x18, [source, dest, 0, 0]);
    assert_eq!(gba.bus.read16(dest), 0x0100);
    assert_eq!(gba.bus.read16(dest + 2), 0x0103);
}

#[test]
fn bit_unpack_expands_one_bit_units_into_nibbles_with_an_offset() {
    let mut gba = machine(&[]);
    let source = EWRAM_BASE + 0x20;
    let dest = IWRAM_BASE + 0x200;
    let info = EWRAM_BASE + 0x10;
    // One source byte of 1-bit units, 4-bit destinations, an offset of 1 and no zero flag: a set bit
    // becomes colour 2, a clear one stays 0.
    put(&mut gba, source, &[0xF0]);
    // "16bit length in bytes, 8bit source width, 8bit destination width, 32bit offset | zero flag".
    let info_bytes: [u8; 8] = {
        let mut bytes = [0u8; 8];
        bytes[0] = 1; // one source byte
        bytes[1] = 0;
        bytes[2] = 1; // 1 bit in
        bytes[3] = 4; // 4 bits out
        bytes[4] = 1; // offset 1
        bytes
    };
    put(&mut gba, info, &info_bytes);
    call(&mut gba, 0x10, [source, dest, info, 0]);
    // Bits are consumed from the least significant end of each byte, so 0xF0 is four zeros and then
    // four set bits: the word is 0x22220000.
    assert_eq!(gba.bus.read32(dest), 0x2222_0000);
}

// ---------------------------------------------------------------- sound and misc

#[test]
fn sound_bias_moves_the_level_and_keeps_the_other_bits() {
    let mut gba = machine(&[]);
    // Post-boot SOUNDBIAS is 0200h. Asking for level 0 clears the low ten bits.
    call(&mut gba, 0x19, [0, 0, 0, 0]);
    assert_eq!(gba.bus.read16(IO_BASE + 0x88), 0x0000);
    // Anything else asks for 200h.
    call(&mut gba, 0x19, [1, 0, 0, 0]);
    assert_eq!(gba.bus.read16(IO_BASE + 0x88), 0x0200);
    // And the upper bits — set by hand — survive.
    gba.bus.write16(IO_BASE + 0x88, 0x8000);
    call(&mut gba, 0x19, [1, 0, 0, 0]);
    assert_eq!(gba.bus.read16(IO_BASE + 0x88), 0x8200);
}

#[test]
fn midi_key_2_freq_is_the_documented_curve() {
    let mut gba = machine(&[]);
    // WaveData.freq at +4 is the sampling rate scaled to key 180, so key 180 plays it as it is and
    // key 168 — twelve keys down — plays it at half.
    let wave = IWRAM_BASE + 0x100;
    gba.bus.write32(wave + 4, 13_379);
    let [at_180, _, _, _] = call(&mut gba, 0x1F, [wave, 180, 0, 0]);
    assert_eq!(at_180, 13_379);
    let [at_168, _, _, _] = call(&mut gba, 0x1F, [wave, 168, 0, 0]);
    assert_eq!(at_168, 6_690, "an octave down is half the frequency");
    let [at_192, _, _, _] = call(&mut gba, 0x1F, [wave, 192, 0, 0]);
    assert_eq!(at_192, 26_758, "and an octave up is twice it");
}

#[test]
fn the_bios_checksum_is_the_published_one() {
    let mut gba = machine(&[]);
    let [checksum, _, _, _] = call(&mut gba, 0x0D, [0, 0, 0, 0]);
    assert_eq!(
        checksum, 0xBAAE_187F,
        "GBATEK: \"The checksum is BAAE187Fh\""
    );
}

// ---------------------------------------------------------------- dispatch, recording, waiting

#[test]
fn an_arm_swi_and_a_thumb_swi_are_the_same_call() {
    // ARM `swi 80000h` on the cartridge: "In ARM mode, only the upper 8bit of the 24bit comment
    // field are interpreted".
    let mut gba = machine(&[0xEF08_0000, 0xEAFF_FFFE]);
    gba.cpu.r[0] = 0x0100;
    gba.step_instruction();
    assert_eq!(gba.bus.bios.calls_to(0x08), 1, "ARM swi 80000h is Sqrt");
    assert_eq!(gba.cpu.r[0], 16);

    // The same call from Thumb, where the comment field *is* the number: `swi 8`.
    let mut gba = machine(&[0xEAFF_FFFE]);
    gba.bus.write16(IWRAM_BASE, 0xDF08); // swi 8
    gba.cpu.set_thumb(true);
    gba.cpu.r[15] = IWRAM_BASE;
    gba.cpu.r[0] = 0x0100;
    gba.step_instruction();
    assert_eq!(
        gba.bus.bios.calls_to(0x08),
        1,
        "Thumb swi 8 is the same call"
    );
    assert_eq!(gba.cpu.r[0], 16);
}

#[test]
fn a_traced_run_records_the_number_the_arguments_and_a_refusal() {
    // `--trace-swi N` (G5b): an ARM `swi 80000h` is Sqrt, and the record is what the *caller* held —
    // R0 is captured before the call writes its result back into it.
    let mut gba = machine(&[0xEF08_0000, 0xEAFF_FFFE]); // swi 80000h / b .
    gba.trace_swis(4);
    gba.cpu.r[0] = 0x0100;
    gba.cpu.r[1] = 0x22;
    gba.step_instruction();
    let trace = gba.swi_trace();
    assert_eq!(trace.len(), 1);
    assert_eq!(trace[0].number, 0x08);
    assert_eq!(trace[0].args[0], 0x0100, "the caller's R0, not the result");
    assert_eq!(trace[0].args[1], 0x22);
    assert_eq!(trace[0].ret, 0x0800_00C4, "the address after the SWI");
    assert!(!trace[0].refused);
    assert_eq!(gba.cpu.r[0], 16, "and the call still returned its answer");

    // Asking again restarts the record, and the new limit caps it.
    gba.trace_swis(1);
    assert!(
        gba.swi_trace().is_empty(),
        "asking again restarts the record"
    );
    call(&mut gba, 0x08, [16, 0, 0, 0]);
    call(&mut gba, 0x08, [16, 0, 0, 0]);
    assert_eq!(gba.swi_trace().len(), 1);

    // A refused number says so, which is the one field a report needs to spot a gap. `$2A` is
    // SoundGetJumpList: 36 more sound routines that GBATEK leaves undocumented, so it is still
    // refused rather than invented.
    let mut gba = machine(&[]);
    gba.trace_swis(4);
    call(&mut gba, 0x2A, [7, 8, 9, 10]);
    let trace = gba.swi_trace();
    assert_eq!(trace.len(), 1);
    assert_eq!(trace[0].number, 0x2A, "SoundGetJumpList");
    assert_eq!(trace[0].args, [7, 8, 9, 10]);
    assert!(trace[0].refused);
}

#[test]
fn an_unimplemented_call_is_recorded_and_the_machine_stops() {
    let mut gba = machine(&[]);
    // $25 is MultiBoot: "recorded-and-refused" per the job's brief. (The sound driver's own entries
    // are implemented now, so the refused set is $25, $26, $2A and everything above $2A.)
    call(&mut gba, 0x25, [0, 0, 0, 0]);
    assert_eq!(gba.bus.bios.unimplemented, Some(0x25));
    // And a number that is not a call at all (GBATEK: "2Bh+ Crash").
    let mut gba = machine(&[]);
    call(&mut gba, 0x40, [0, 0, 0, 0]);
    assert_eq!(gba.bus.bios.unimplemented, Some(0x40));
    // A call that *is* implemented leaves no record.
    let mut gba = machine(&[]);
    call(&mut gba, 0x08, [16, 0, 0, 0]);
    assert_eq!(gba.bus.bios.unimplemented, None);
    assert_eq!(gba.bus.bios.calls_to(0x08), 1);
}

#[test]
fn the_jump_list_refusal_writes_nothing_and_stops_the_machine() {
    // `$2A` SoundGetJumpList asks the BIOS to fill `120h` bytes at `R0` with 36 sound-routine
    // pointers. GBATEK gives the call and the buffer's size but names none of the 36 entries, and
    // no cartridge in the library asks for it (0 of 866 over 1 200 frames, traced), so the
    // dispatcher refuses rather than inventing addresses a caller could not tell from the real
    // ones: the destination keeps every byte it had.
    const BUFFER: u32 = IWRAM_BASE + 0x400;
    let mut gba = machine(&[0xEF2A_0000]); // swi 2A0000h (SoundGetJumpList)
    let before = vec![0x5A; 0x120];
    put(&mut gba, BUFFER, &before);
    gba.cpu.r[0] = BUFFER;
    gba.step_instruction();
    assert_eq!(
        get(&mut gba, BUFFER, 0x120),
        before,
        "the 120h-byte buffer is untouched"
    );
    assert_eq!(gba.bus.bios.calls_to(0x2A), 1, "the call was counted");
    assert_eq!(gba.bus.bios.unimplemented, Some(0x2A));
    assert!(
        gba.cpu.halted,
        "and the machine stops rather than run on as if it had worked"
    );
}

#[test]
fn halt_waits_for_an_enabled_interrupt_even_with_ime_clear() {
    let mut gba = machine(&[0xEF02_0000, 0xEAFF_FFFE]); // swi 20000h (Halt) / b .
    // IE bit 0 enabled, nothing pending yet.
    gba.bus.write16(IO_BASE + 0x200, 1);
    gba.step_instruction(); // the SWI
    assert!(gba.cpu.halted, "Halt put the core to sleep");
    // "the state of CPUs IRQ disable bit in CPSR register, and the IME register are don't care."
    assert_eq!(gba.bus.io[0x208], 0, "IME is clear");
    for _ in 0..10 {
        gba.step_instruction();
        assert!(gba.cpu.halted, "nothing has been requested");
    }
    gba.bus.raise_interrupt(1);
    gba.step_instruction();
    assert!(!gba.cpu.halted, "the request woke the core");
}

#[test]
fn intr_wait_sets_ime_waits_for_the_flag_word_and_clears_it() {
    let mut gba = machine(&[]);
    // IME is clear; IntrWait "forcefully sets IME=1".
    call(&mut gba, 0x04, [1, 1, 0, 0]); // discard old flags, watch V-Blank
    assert_eq!(gba.bus.read16(IO_BASE + 0x208), 1, "IME was set");
    assert!(gba.cpu.halted, "the call is waiting");
    assert_eq!(gba.bus.bios.wait, Some(1));

    // Nothing has happened yet.
    gba.step_instruction();
    assert!(gba.cpu.halted);

    // The flag word is what the interrupt handler keeps; the wait ends when it holds the flag.
    gba.bus.write16(0x0300_7FF8, 0x0001);
    gba.step_instruction();
    assert!(!gba.cpu.halted, "the wait finished");
    assert_eq!(gba.bus.bios.wait, None);
    assert_eq!(
        gba.bus.read16(0x0300_7FF8),
        0,
        "the flag it watched was cleared"
    );

    // `R0 = 0` returns at once when the flag is already there.
    gba.bus.write16(0x0300_7FF8, 0x0002);
    call(&mut gba, 0x04, [0, 2, 0, 0]);
    assert!(!gba.cpu.halted);
    assert_eq!(gba.bus.read16(0x0300_7FF8), 0);
}

#[test]
fn vblank_intr_wait_is_intr_wait_for_one_flag() {
    let mut gba = machine(&[]);
    call(&mut gba, 0x05, [0, 0, 0, 0]);
    assert_eq!(gba.bus.bios.wait, Some(1));
    assert!(gba.cpu.halted);
}

/// The discard argument, quoted where GBATEK prints it (*SWI 04h - IntrWait*): "r0    0=Return
/// immediately if an old flag was already set (NDS9: bugged!) / 1=Discard old flags, wait until a
/// NEW flag becomes set"; "r1    Interrupt flag(s) to wait for (same format as IE/IF registers)";
/// "Return: No return value, the selected flag(s) are automatically reset in BIOS Interrupt Flags
/// value in RAM upon return."
#[test]
fn intr_wait_follows_the_documented_discard_rule() {
    let mut gba = machine(&[]);
    // Two flags are already in the RAM word the user's handler maintains. `r0=0` must not discard
    // them, must return at once on the one it watches, and must reset only that one.
    gba.bus.write16(0x0300_7FF8, 0x0003);
    let answer = call(&mut gba, 0x04, [0, 1, 0, 0]);
    assert!(
        !gba.cpu.halted,
        "r0=0 returns at once when an old flag is set"
    );
    assert_eq!(gba.bus.bios.wait, None, "and nothing is left waiting");
    assert_eq!(
        gba.bus.read16(0x0300_7FF8),
        0x0002,
        "the flag it watched is reset and the one it did not is left alone"
    );
    assert_eq!(answer[0], 0, "there is no return value: R0 is the argument");

    // `r0=1` discards the old flags first, so the same already-set flag cannot satisfy the call: it
    // has to wait for a NEW one.
    gba.bus.write16(0x0300_7FF8, 0x0001);
    call(&mut gba, 0x04, [1, 1, 0, 0]);
    assert_eq!(
        gba.bus.read16(0x0300_7FF8),
        0,
        "the old flags were discarded"
    );
    assert!(gba.cpu.halted, "and the call waits for a new one");
    assert_eq!(gba.bus.bios.wait, Some(1));

    // The new flag is what finishes it, through the same reset.
    gba.bus.write16(0x0300_7FF8, 0x0001);
    gba.step_instruction();
    assert!(!gba.cpu.halted, "the new flag finished the call");
    assert_eq!(gba.bus.bios.wait, None);
    assert_eq!(gba.bus.read16(0x0300_7FF8), 0, "and was reset");
}

/// `VBlankIntrWait` is `IntrWait(1, 1)` by construction (*SWI 05h*: "The function sets r0=1 and r1=1
/// (plus r2=0 on DSi7) and does then execute IntrWait (SWI 04h)"), so an old V-Blank flag must be
/// discarded rather than returned on.
#[test]
fn vblank_intr_wait_discards_the_old_flag_and_waits_for_a_new_one() {
    let mut gba = machine(&[]);
    gba.bus.write16(0x0300_7FF8, 1);
    call(&mut gba, 0x05, [0, 0, 0, 0]);
    assert_eq!(
        gba.bus.read16(0x0300_7FF8),
        0,
        "r0=1: the old flag was discarded"
    );
    assert!(gba.cpu.halted, "it did not return on the old flag");
    assert_eq!(gba.bus.bios.wait, Some(1));
}

/// An interrupt the call did not ask for does not end it: *SWI 04h* "Continues to wait in Halt state
/// until one (or more) of the specified interrupt(s) do occur", where `Halt` alone ends at "any
/// enabled interrupts ... that is when (IE AND IF) is not zero" (*BIOS Halt Functions*).
///
/// A cartridge that enables the H-Blank request as well as the V-Blank one — `0534` does, `IE 0007h`
/// with `DISPSTAT 2038h` — would otherwise come back from every `IntrWait(0, 1)` about one line into
/// the sleep, one hundred and sixty times a frame, and the flag word its handler filled at the
/// V-Blank would then be consumed by a call that had already returned.
#[test]
fn an_interrupt_the_call_did_not_ask_for_does_not_end_intr_wait() {
    // `mov r0,#0` / `mov r1,#1` / `swi 40000h` / `mov r0,#1` / `b .`: the marker is what proves the
    // instruction after the `SWI` ran, and R0 is the discard argument the call must not change.
    let mut gba = machine(&[
        0xE3A0_0000, // mov r0,#0
        0xE3A0_1001, // mov r1,#1     - watch the V-Blank
        0xEF04_0000, // swi 40000h    - IntrWait
        0xE3A0_0001, // mov r0,#1     - the marker
        0xEAFF_FFFE, // b .
    ]);
    // A user handler that acknowledges every pending request and returns to the BIOS handler — the
    // pattern GBATEK's own sample handler prints.
    let handler = IWRAM_BASE + 0x800;
    for (index, word) in [
        0xE3A0_0404, // mov r0,#$04000000
        0xE280_0C02, // add r0,r0,#$200    - IE
        0xE1D0_10B2, // ldrh r1,[r0,#2]    - IF
        0xE1C0_10B2, // strh r1,[r0,#2]    - acknowledge what is pending
        0xE12F_FF1E, // bx lr
    ]
    .into_iter()
    .enumerate()
    {
        gba.bus.write32(handler + index as u32 * 4, word);
    }
    gba.bus.write32(0x0300_7FFC, handler);
    gba.bus.write16(IO_BASE + 0x004, (1 << 3) | (1 << 4)); // DISPSTAT: V-Blank and H-Blank IRQ
    gba.bus.write16(IO_BASE + 0x200, 0b11); // IE: both

    for _ in 0..3 {
        gba.step_instruction(); // mov r0, mov r1, the `swi`
    }
    assert_eq!(gba.bus.bios.calls_to(0x04), 1, "the call ran");
    assert_eq!(gba.bus.bios.wait, Some(1), "it waits for the V-Blank");
    assert!(gba.cpu.halted, "and the core sleeps");
    let parked = gba.cpu.pc();
    assert_eq!(gba.cpu.r[0], 0, "the marker has not run");

    // Walk the clock to the first H-Blank, which is a request the call did not ask for: its handler
    // runs, and the call must still be waiting when it returns to the parked instruction.
    let mut steps = 0;
    while gba.bus.read16(0x0300_7FF8) & 0b10 == 0 && steps < 10_000 {
        gba.step_instruction();
        steps += 1;
    }
    assert_eq!(
        gba.bus.read16(0x0300_7FF8) & 0b10,
        0b10,
        "an H-Blank was requested and the handler saw it"
    );
    while gba.cpu.pc() != parked && steps < 10_000 {
        gba.step_instruction();
        steps += 1;
    }
    assert_eq!(
        gba.cpu.pc(),
        parked,
        "the handler returned to the parked instruction"
    );
    assert_eq!(
        gba.bus.bios.wait,
        Some(1),
        "the call is still waiting: it did not ask for the H-Blank"
    );
    assert_eq!(gba.cpu.r[0], 0, "and the marker still has not run");
    assert!(gba.cpu.halted, "the caller is asleep again");

    // The flag the call did ask for ends it, through the same RAM word.
    while gba.cpu.r[0] != 1 && steps < 400_000 {
        gba.step_instruction();
        steps += 1;
    }
    assert_eq!(gba.cpu.r[0], 1, "the V-Blank flag finished the call");
    assert_eq!(gba.bus.bios.wait, None);
    assert_eq!(
        gba.bus.read16(0x0300_7FF8) & 1,
        0,
        "and the flag it watched was reset on the way out"
    );
}

#[test]
fn vblank_intr_wait_returns_on_the_next_vblank_without_a_busy_loop() {
    // A cartridge that calls VBlankIntrWait once and then sets a marker: `swi 50000h` / `mov r0,#1` /
    // `b .`. The call runs on the first step (the CPU is halted from the end of it) and the flag its
    // handler leaves finishes it; the clock is driven by hand so the frame this lands in is exact.
    let mut gba = machine(&[0xEF05_0000, 0xE3A0_0001, 0xEAFF_FFFE]);
    // A user handler that acknowledges the V-Blank and returns to the BIOS handler, as the suite's
    // own handler does. `$04000200` is not an ARM immediate, so the register is built in two
    // instructions; `IF` is the halfword two bytes on from there.
    let handler = IWRAM_BASE + 0x800;
    for (index, word) in [
        0xE3A0_0404, // mov r0,#$04000000
        0xE280_0C02, // add r0,r0,#$200      - IE
        0xE3A0_1001, // mov r1,#1
        0xE1C0_10B2, // strh r1,[r0,#2]      - $04000202, acknowledge IF bit 0
        0xE12F_FF1E, // bx lr
    ]
    .into_iter()
    .enumerate()
    {
        gba.bus.write32(handler + index as u32 * 4, word);
    }
    gba.bus.write32(0x0300_7FFC, handler);
    gba.bus.write16(IO_BASE + 0x004, 1 << 3); // DISPSTAT: V-Blank IRQ
    gba.bus.write16(IO_BASE + 0x200, 1); // IE: V-Blank

    let mut steps = 0;
    gba.step_instruction(); // the `swi`
    assert_eq!(gba.bus.bios.calls_to(0x05), 1, "the call ran");
    assert_eq!(gba.bus.bios.wait, Some(1), "it is waiting for the V-Blank");
    assert!(gba.cpu.halted, "the core sleeps, it does not spin");

    // Halt means halt: a thousand turns of a halted core execute nothing and move nothing. There is
    // no loop in the cartridge's code watching `IF` — the machine is asleep, not busy.
    let parked = (gba.cpu.pc(), gba.cpu.instructions);
    for _ in 0..1000 {
        gba.step_instruction();
    }
    assert_eq!(
        (gba.cpu.pc(), gba.cpu.instructions),
        parked,
        "the halted core ran an instruction"
    );
    assert_eq!(gba.cpu.r[0], 0, "and the call has not returned yet");

    // The frame clock still advances while the CPU sleeps; the test takes it to the V-Blank by hand
    // rather than stepping a halted core 160 lines' worth of cycles.
    let started = gba.bus.cycles;
    gba.bus.tick(160 * 1232);
    assert_eq!(
        gba.bus.read16(IO_BASE + 0x202) & 1,
        1,
        "the clock reached line 160"
    );
    while gba.cpu.r[0] != 1 && steps < 200 {
        gba.step_instruction();
        steps += 1;
    }
    let waited = gba.bus.cycles - started;
    println!(
        "VBlankIntrWait: woke after {steps} steps ({waited} cycles), PC {:08X}",
        gba.cpu.pc()
    );
    assert_eq!(gba.cpu.r[0], 1, "the machine came back from the wait");
    assert_eq!(gba.bus.bios.wait, None, "and the wait is over");
    assert_eq!(
        gba.bus.read16(IO_BASE + 0x202) & 1,
        0,
        "the handler acknowledged the V-Blank"
    );
    // It slept through the frame's visible lines and returned on the V-Blank, through the BIOS IRQ
    // handler and the user's routine, inside the frame it waited in.
    assert!(waited >= 160u64 * 1232, "it woke before V-Blank");
    assert!(
        waited < u64::from(CYCLES_PER_FRAME),
        "and inside the frame it waited in"
    );
    assert!(steps < 50, "the wake cost {steps} steps, which is a spin");
}

#[test]
fn the_bios_record_is_in_the_save_state() {
    let mut gba = machine(&[0xEAFF_FFFE]);
    call(&mut gba, 0x08, [16, 0, 0, 0]);
    call(&mut gba, 0x25, [0, 0, 0, 0]); // MultiBoot: refused
    let saved = gba.save_state();
    assert_eq!(gba.bus.bios.unimplemented, Some(0x25));
    gba.bus.bios = Bios::new();
    gba.load_state(&saved).expect("our own state");
    assert_eq!(gba.bus.bios.calls_to(0x08), 1, "the call count came back");
    assert_eq!(
        gba.bus.bios.unimplemented,
        Some(0x25),
        "and so did the call it refused"
    );
    assert_eq!(gba.bus.bios.latch, OPCODE_AFTER_SWI);
}

// ---------------------------------------------------------------- the reset

/// The IWRAM `RegisterRamReset` leaves alone: "Clear 32K on-chip WRAM ;-excluding last 200h
/// bytes", the area the call's own note calls "the CPU internal RAM area from 3007E00h-3007FFFh".
const IWRAM_KEPT: usize = 0x8000 - 0x200;

#[test]
fn register_ram_reset_charges_the_clears_to_the_clock_and_leaves_the_last_200h() {
    // GBATEK, *SWI 01h (GBA) - RegisterRamReset*: "Resets the I/O registers and RAM specified in
    // ResetFlags. However, it does not clear the CPU internal RAM area from 3007E00h-3007FFFh.",
    // with the flags "0 Clear 256K on-board WRAM", "1 Clear 32K on-chip WRAM ;-excluding last 200h
    // bytes", "2 Clear Palette", "3 Clear VRAM", "4 Clear OAM ;-zerofilled! does NOT disable
    // OBJs!" and "The function always switches the screen into forced blank by setting
    // DISPCNT=0080h (regardless of incoming R0, screen becomes white)".
    //
    // The clear is the BIOS's own memory fill — "Memfill as single LDR followed by repeated
    // STMIA [Rb]!,r2-r9" (GBATEK, *SWI 0Ch - CpuFastSet*) — so every word costs one 32-bit CPU
    // store at its region's row of *Address Bus Width and CPU Read/Write Access Widths*: 256K at
    // `3/3/6` is 65 536 × 6 = 393 216, 32K less 200h at `1/1/1` is 8 064 × 1, palette and VRAM at
    // `1/1/2` are 256 × 2 and 24 576 × 2, OAM at `1/1/1` is 256 × 1. Those cycles are the
    // machine's, so they are on the clock when the call returns (it returns none of its own).
    for (flags, clock, phase) in [
        (0x01u32, 393_216u32, "256K on-board WRAM, 65 536 words at 6"),
        (
            0x02,
            8_064,
            "32K on-chip WRAM less its last 200h, 8 064 words at 1",
        ),
        (0x04, 512, "the palette, 256 words at 2"),
        (0x08, 49_152, "VRAM, 24 576 words at 2"),
        (0x10, 256, "OAM, 256 words at 1"),
        (0x03, 401_280, "both work RAMs: 393 216 + 8 064"),
    ] {
        let mut gba = machine(&[]);
        gba.bus.ewram.fill(0xFF);
        gba.bus.iwram.fill(0xFF);
        gba.bus.palette.fill(0xFF);
        gba.bus.vram.fill(0xFF);
        gba.bus.oam.fill(0xFF);
        // `$03007FFC`, "Pointer to user IRQ handler (32bit ARM code)", is inside the 200h that stay.
        gba.bus.write32(0x0300_7FFC, 0x0300_1B80);
        // In V-Blank, so the video memories' row carries the table's no `*` — the `*` is "Plus 1
        // cycle if GBA accesses video memory at the same time" and a force-blanked screen does not.
        gba.bus.lcd.line = VBLANK_FIRST_LINE;
        gba.bus.lcd.line_cycle = 0;
        gba.cpu.r[0] = flags;
        let before = gba.bus.cycles;
        assert_eq!(
            bios::call(&mut gba.cpu, &mut gba.bus, 0x01),
            0,
            "the clears' cycles are on the clock, not returned for the `SWI` to spend"
        );
        assert_eq!(
            gba.bus.cycles - before,
            u64::from(clock),
            "R0 bit(s) {flags:02X} cost {phase}"
        );
        assert_eq!(
            gba.bus.read16(IO_BASE + DISPCNT as u32),
            0x0080,
            "\"The function always switches the screen into forced blank by setting DISPCNT=0080h\""
        );
        // Exactly the regions the flags name, and nothing else.
        let cleared = u32::from(gba.bus.ewram.iter().all(|&byte| byte == 0))
            | u32::from(gba.bus.iwram[..IWRAM_KEPT].iter().all(|&byte| byte == 0)) << 1
            | u32::from(gba.bus.palette.iter().all(|&byte| byte == 0)) << 2
            | u32::from(gba.bus.vram.iter().all(|&byte| byte == 0)) << 3
            | u32::from(gba.bus.oam.iter().all(|&byte| byte == 0)) << 4;
        assert_eq!(cleared, flags, "the flags decide which regions go");
        assert_eq!(
            gba.bus.read32(0x0300_7FFC),
            0x0300_1B80,
            "the last 200h of the 32K survive, so $03007FFC keeps the handler it held"
        );
        assert!(
            gba.bus.iwram[IWRAM_KEPT..0x7FFC]
                .iter()
                .all(|&byte| byte == 0xFF),
            "byte for byte, $03007E00-$03007FFF is left alone"
        );
    }
}

#[test]
fn a_v_blank_that_arrives_during_the_256k_clear_is_taken_before_the_32k_one() {
    // GBATEK, *How BIOS Processes SWIs*: "Each time when calling a BIOS function 4 words (SPSR,
    // R11, R12, R14) are saved on Supervisor stack (_svc). Once it has saved that data, the SWI
    // handler switches into System mode", and "In some cases the BIOS may allow interrupts to be
    // executed from inside of the SWI procedure." This slice clears the 256K first and the 32K —
    // where a cartridge keeps the interrupt handler it is about to erase — last (GBATEK prints no
    // order; mGBA's `_RegisterRamReset` in `src/gba/bios.c` clears the 256K before the 32K too),
    // so a V-Blank that arrives during the 256K clear is serviced by a handler that is still there.
    let mut gba = machine(&[]);
    // The handler, in the 32K the call erases last: `mov r4,#1` / `bx lr` — two words, not three.
    // It does not acknowledge `IF` itself; the test writes `IF` from the test side after the handler
    // has run, because the handler's only job here is to be *there* when the interrupt arrives.
    // R4 is not one of the registers the BIOS's IRQ entry saves, so a set bit is the handler's mark
    // on the machine.
    for (index, word) in [0xE3A0_4001u32, 0xE12F_FF1E].into_iter().enumerate() {
        let at = 0x100 + index * 4;
        gba.bus.iwram[at..at + 4].copy_from_slice(&word.to_le_bytes());
    }
    gba.bus.write32(0x0300_7FFC, 0x0300_0100);
    // The interrupt the reset will run into: V-Blank IRQ enable, its `IE` bit, and `IME`.
    gba.bus.write16(IO_BASE + DISPSTAT as u32, 0x0008);
    gba.bus.write16(IO_BASE + IE as u32, 0x0001);
    gba.bus.write16(IO_BASE + IME as u32, 1);
    // One line before V-Blank, so the flag rises 1 232 cycles into a 393 216-cycle clear.
    gba.bus.lcd.line = VBLANK_FIRST_LINE - 1;
    gba.bus.lcd.line_cycle = 0;
    gba.cpu.r[0] = 0x03; // 256K on-board and 32K on-chip

    let before = gba.bus.cycles;
    assert_eq!(
        bios::call(&mut gba.cpu, &mut gba.bus, 0x01),
        0,
        "the clears' cycles are on the clock, not returned for the `SWI` to spend"
    );
    assert_eq!(
        gba.bus.cycles - before,
        u64::from(393_216u32),
        "the call spent the 256K clear's cycles on the clock and stopped there"
    );
    assert_eq!(
        gba.bus.irq_flags() & 1,
        1,
        "a V-Blank arrived inside the 256K clear"
    );
    assert_eq!(
        gba.bus.iwram[0x100..0x104],
        [0x01, 0x40, 0xA0, 0xE3],
        "the handler the 32K clear will erase is still there when the interrupt is due"
    );

    // The next instruction boundary takes the interrupt: the BIOS vector at `$18`, then the
    // cartridge's handler. The handler acknowledges the flag the way a cartridge's does — this one
    // writes `IF` from the test rather than from an instruction, since a handler's own job here is
    // only to be *there* when the interrupt arrives — and the parked 32K clear then runs before
    // the caller's own instruction.
    for _ in 0..4 {
        gba.step_instruction();
    }
    assert_eq!(gba.cpu.r[4], 1, "the V-Blank handler ran");
    gba.bus.write16(IO_BASE + 0x202, 0x3FFF); // acknowledge every flag, as a handler does
    for _ in 0..8 {
        gba.step_instruction();
    }
    assert!(
        gba.bus.iwram[..IWRAM_KEPT].iter().all(|&byte| byte == 0),
        "and the 32K was cleared after it returned, not before it ran"
    );
    assert_eq!(
        gba.bus.read32(0x0300_7FFC),
        0x0300_0100,
        "$03007FFC is inside the last 200h, so the pointer survives the clear"
    );
}

/// A state taken with a `RegisterRamReset` parked must come back with the regions it has not cleared
/// yet and the address it resumes at: `Bios::STATE_LEN` grew by those two words in version 12, so a
/// resumed machine neither re-clears what is already gone nor runs the caller out of the call.
#[test]
fn the_parked_reset_is_in_the_save_state() {
    let mut gba = machine(&[]);
    gba.bus.iwram.fill(0xFF);
    // V-Blank IRQ enabled and open, one line before the flag: the 256K clear is cut short.
    gba.bus.write16(IO_BASE + DISPSTAT as u32, 0x0008);
    gba.bus.write16(IO_BASE + IE as u32, 0x0001);
    gba.bus.write16(IO_BASE + IME as u32, 1);
    gba.bus.lcd.line = VBLANK_FIRST_LINE - 1;
    gba.bus.lcd.line_cycle = 0;
    gba.cpu.r[0] = 0x03; // 256K on-board and 32K on-chip
    assert_eq!(bios::call(&mut gba.cpu, &mut gba.bus, 0x01), 0);
    assert_eq!(gba.bus.bios.reset_pending, 0x02, "the 32K clear is parked");
    assert_ne!(gba.bus.bios.reset_pc, 0, "with the address it resumes at");
    assert!(
        gba.bus.iwram[..IWRAM_KEPT].iter().all(|&byte| byte == 0xFF),
        "and the 32K is still there to clear"
    );
    let parked_resume = gba.bus.bios.reset_pc;

    let saved = gba.save_state();
    gba.bus.bios.reset_pending = 0;
    gba.bus.bios.reset_pc = 0;
    gba.load_state(&saved).expect("our own state");
    assert_eq!(
        gba.bus.bios.reset_pending, 0x02,
        "the regions still to clear came back with the state"
    );
    assert_eq!(
        gba.bus.bios.reset_pc, parked_resume,
        "and so did the address the call resumes at"
    );
}

/// A state taken while `IntrWait` is still waiting must come back still waiting at the same
/// instruction. `Bios::STATE_LEN` grew by the parked address in version 10 (`lib.rs`'s
/// `STATE_VERSION`), so this is the round trip that proves the new field is in the payload and that
/// a resumed machine does not run the caller out of a call that has not returned.
#[test]
fn the_parked_intr_wait_is_in_the_save_state() {
    // `mov r0,#1` / `mov r1,#1` / `swi 40000h`: discard the old flags and wait for a new V-Blank,
    // which never arrives because no interrupt is enabled in this test.
    let mut gba = machine(&[
        0xE3A0_0001, // mov r0,#1
        0xE3A0_1001, // mov r1,#1
        0xEF04_0000, // swi 40000h
    ]);
    for _ in 0..3 {
        gba.step_instruction();
    }
    let parked = gba.cpu.pc();
    assert_ne!(parked, 0, "the call parked the caller past the SWI");
    assert!(gba.cpu.halted);
    assert_eq!(gba.bus.bios.wait, Some(1));

    let saved = gba.save_state();
    gba.bus.bios = Bios::new();
    gba.bus.bios.wait_pc = 0;
    gba.load_state(&saved).expect("our own state");
    assert_eq!(
        gba.bus.bios.wait,
        Some(1),
        "the flag mask came back with the call"
    );
    assert_eq!(
        gba.bus.bios.wait_pc, parked,
        "and so did the instruction the caller is parked at"
    );
}

// ---------------------------------------------------------------- the interrupt handler

#[test]
fn an_irq_reaches_the_user_handler_through_the_bios_handler() {
    let mut gba = machine(&[0xEAFF_FFFE]); // b .
    // A user handler in IWRAM: read the BIOS, store it, return to the BIOS.
    let handler = IWRAM_BASE + 0x400;
    let program = [
        0xE3A0_0000, // mov r0,#0
        0xE590_1000, // ldr r1,[r0]      - the BIOS read the suite makes
        0xE3A0_2403, // mov r2,#$03000000
        0xE582_1000, // str r1,[r2]
        0xE12F_FF1E, // bx lr            - back to the BIOS handler
    ];
    for (index, word) in program.into_iter().enumerate() {
        gba.bus.write32(handler + index as u32 * 4, word);
    }
    gba.bus.write32(0x0300_7FFC, handler);

    let sp_before = gba.cpu.r[13];
    gba.bus.write16(IO_BASE + 0x200, 1); // IE: V-Blank
    gba.bus.write16(IO_BASE + 0x208, 1); // IME
    let interrupted = gba.cpu.pc();
    gba.bus.raise_interrupt(1);

    gba.step_instruction();
    assert_eq!(gba.cpu.pc(), cpu::VECTOR_IRQ, "the IRQ vector");
    gba.step_instruction();
    assert_eq!(
        gba.cpu.pc(),
        bios::IRQ_HANDLER,
        "and its branch to the handler"
    );
    gba.step_instruction();
    assert_eq!(
        gba.cpu.mode(),
        cpu::MODE_IRQ,
        "the handler runs in IRQ mode"
    );
    assert_eq!(gba.cpu.pc(), handler, "the user's handler was entered");
    assert_eq!(gba.cpu.r[0], IO_BASE, "\"mov r0,4000000h\"");
    assert_eq!(gba.cpu.r[14], bios::IRQ_EPILOGUE, "\"add r14,r15,0h\"");
    assert_eq!(
        gba.bus.bios.latch, OPCODE_DURING_IRQ,
        "the BIOS read during an IRQ returns [0134h+8]"
    );
    // The six registers are on the IRQ stack, and SP_irq is 24 bytes below where it was.
    assert_eq!(gba.cpu.r[13], cpu::SP_IRQ - 24, "six words pushed");
    assert_eq!(
        gba.bus.read32(gba.cpu.r[13] + 20),
        interrupted + 4,
        "LR_irq"
    );

    for _ in 0..6 {
        gba.step_instruction();
    }
    assert_eq!(gba.bus.read32(IWRAM_BASE), OPCODE_DURING_IRQ);
    // The handler stored the BIOS read it made, then the epilogue returned to the interrupted
    // instruction with the interrupted mode restored — the boot mode, System (GBATEK, *GBA
    // Cartridge Header*: "With all entry points, the CPU is initially set into system mode").
    assert_eq!(
        gba.cpu.mode(),
        cpu::MODE_SYS,
        "back where the IRQ came from"
    );
    assert_eq!(
        gba.cpu.pc(),
        interrupted,
        "and at the instruction it interrupted"
    );
    assert_eq!(gba.cpu.r[13], sp_before, "SP_sys is the caller's again");
    assert_eq!(
        gba.bus.bios.latch, OPCODE_AFTER_IRQ,
        "a read after the IRQ returns [013Ch+8]"
    );
}

#[test]
fn an_irq_takes_the_flag_with_it_for_intr_wait() {
    let mut gba = machine(&[0xEAFF_FFFE]);
    let handler = IWRAM_BASE + 0x400;
    gba.bus.write32(handler, 0xE12F_FF1E); // bx lr
    gba.bus.write32(0x0300_7FFC, handler);
    gba.bus.write16(IO_BASE + 0x200, 1);
    gba.bus.write16(IO_BASE + 0x208, 1);
    gba.bus.raise_interrupt(1);
    for _ in 0..3 {
        gba.step_instruction();
    }
    // The handler sampled `IE & IF` into the flag word, which is where `IntrWait` looks.
    assert_eq!(gba.bus.read16(0x0300_7FF8) & 1, 1);
}

// ---------------------------------------------------------------- the BIOS as code

#[test]
fn a_jump_to_a_bios_address_we_do_not_implement_is_recorded() {
    let mut gba = machine(&[0xEAFF_FFFE]);
    gba.cpu.r[15] = 0x0200;
    let retired = gba.cpu.instructions;
    gba.step_instruction();
    assert_eq!(gba.bus.bios.unknown_entry, Some(0x0200));
    assert!(
        !gba.cpu.halted,
        "the core is not parked: an unimplemented BIOS entry is not a fault"
    );
    assert_eq!(gba.cpu.r[15], 0x0200, "and nothing is fetched from it");
    assert_eq!(
        gba.cpu.instructions, retired,
        "no instruction retires there"
    );
}

#[test]
fn the_bios_latch_moves_with_the_sixteen_bit_call_number() {
    let mut gba = machine(&[]);
    call(&mut gba, 0x08, [16, 0, 0, 0]);
    assert_eq!(gba.bus.bios.latch, OPCODE_AFTER_SWI);
    // A read of the region hands that word back, not the data bus.
    assert_eq!(gba.bus.read32(0), OPCODE_AFTER_SWI);
    // And the boot value is the one a cartridge sees before any call.
    let mut gba = machine(&[]);
    assert_eq!(gba.bus.read32(0), OPCODE_AFTER_BOOT);
}

// ---------------------------------------------------------------- memory timings

/// A machine with a 64 KiB cartridge, for the timing tests.
fn timing_machine() -> Gba {
    let mut rom = vec![0u8; 0x1_0000];
    rom[0..4].copy_from_slice(&0xEA00_002Eu32.to_le_bytes());
    rom[0xA0..0xA9].copy_from_slice(b"GBA-TIME\0");
    rom[0xAC..0xB0].copy_from_slice(b"TTIM");
    rom[0xB0..0xB2].copy_from_slice(b"00");
    rom[0xB2] = 0x96;
    let mut x: u8 = 0;
    for &byte in &rom[0xA0..=0xBC] {
        x = x.wrapping_sub(byte);
    }
    rom[0xBD] = x.wrapping_sub(0x19);
    machine_with_rom(rom)
}

/// The cycles one access really costs, measured through the bus: the single cycle the core's model
/// counts for it, plus what the bus said was owed.
fn access_cycles(gba: &mut Gba, addr: u32, width: usize) -> u32 {
    gba.bus.waits = 0;
    match width {
        1 => {
            gba.bus.read8(addr);
        }
        2 => {
            gba.bus.read16(addr);
        }
        _ => {
            gba.bus.read32(addr);
        }
    }
    1 + gba.bus.waits
}

/// The same, for a *first* (non-sequential) access: the address bookkeeping is cleared so the
/// measurement is of the non-sequential cost and not of a lucky sequential one.
fn first_access(gba: &mut Gba, addr: u32, width: usize) -> u32 {
    gba.bus.last_data_end = None;
    access_cycles(gba, addr, width)
}

#[test]
fn the_internal_memories_cost_what_gbateks_table_says() {
    let mut gba = timing_machine();
    // The rows below are the table's own figures. Three of them carry its `*` footnote — "Plus 1
    // cycle if GBA accesses video memory at the same time" — so the clock is put in V-Blank first,
    // where the display controller is not drawing and the table's figure is what stands. The extra
    // cycle during drawing has its own test (`tests/gba_timing.rs`).
    gba.bus.lcd.line = 160;
    gba.bus.lcd.line_cycle = 0;
    // "Work RAM 32K 32 8/16/32 8/16/32 1/1/1", "I/O 1/1/1", "BIOS ROM 1/1/1", "OAM 1/1/1".
    for (addr, name) in [
        (IWRAM_BASE, "IWRAM"),
        (IO_BASE + 0x130, "I/O"),
        (OAM_BASE, "OAM"),
        (0, "BIOS"),
    ] {
        for width in [1, 2, 4] {
            assert_eq!(
                first_access(&mut gba, addr, width),
                1,
                "{name} at {width} bytes"
            );
        }
    }
    // "Work RAM 256K 16 8/16/32 8/16/32 3/3/6".
    assert_eq!(first_access(&mut gba, EWRAM_BASE, 1), 3, "EWRAM byte");
    assert_eq!(first_access(&mut gba, EWRAM_BASE, 2), 3, "EWRAM halfword");
    assert_eq!(first_access(&mut gba, EWRAM_BASE, 4), 6, "EWRAM word");
    // "Palette RAM 16 ... 1/1/2 *", "VRAM 16 ... 1/1/2 *".
    for (base, name) in [(PALETTE_BASE, "palette"), (VRAM_BASE, "VRAM")] {
        assert_eq!(first_access(&mut gba, base, 1), 1, "{name} byte");
        assert_eq!(first_access(&mut gba, base, 2), 1, "{name} halfword");
        assert_eq!(first_access(&mut gba, base, 4), 2, "{name} word");
    }
}

#[test]
fn the_post_boot_waitcnt_is_the_one_gbatek_gives_for_nintendo_cartridges() {
    let mut gba = timing_machine();
    // "WS0/ROM=3,1 clks; SRAM=8 clks; WS2/EEPROM: 8,8 clks; prefetch enabled; that is
    // WAITCNT=4317h".
    assert_eq!(gba.bus.waitcnt(), 0x4317);
    assert!(gba.bus.prefetch_enabled());
    // With 3 and 1 wait states a ROM access is 4 and 2 cycles.
    assert_eq!(
        first_access(&mut gba, ROM_BASE, 2),
        4,
        "non-sequential halfword"
    );
    assert_eq!(
        first_access(&mut gba, ROM_BASE, 4),
        6,
        "and a word is 4 + 2"
    );
    // SRAM's field is bits 0-1; 3 is eight waits, so nine cycles.
    assert_eq!(first_access(&mut gba, SRAM_BASE, 1), 9);
}

#[test]
fn the_whole_waitcnt_fits_the_table_with_its_power_on_default() {
    let mut gba = timing_machine();
    // WAITCNT = 0000h is GBATEK's *power-on* default: 4,2 wait states and prefetch off, which the
    // memory map's table gives as "5/5/8" for a Game Pak access.
    gba.bus.write16(IO_BASE + WAITCNT as u32, 0x0000);
    assert_eq!(first_access(&mut gba, ROM_BASE, 1), 5, "byte");
    assert_eq!(first_access(&mut gba, ROM_BASE, 2), 5, "halfword");
    assert_eq!(first_access(&mut gba, ROM_BASE, 4), 8, "word: 5 + 3");
    assert_eq!(first_access(&mut gba, SRAM_BASE, 1), 5, "SRAM, 4 + 1");
    // SRAM's field 3 is eight waits: nine cycles.
    gba.bus.write16(IO_BASE + WAITCNT as u32, 0x0003);
    assert_eq!(first_access(&mut gba, SRAM_BASE, 1), 9);
}

#[test]
fn each_of_the_three_windows_has_its_own_wait_states() {
    let mut gba = timing_machine();
    // WS0 = 3,1; WS1 = 4,4 (first field 0, second bit 0); WS2 = 8,8 (first field 3, second bit 0);
    // SRAM 8; prefetch off. That is 0x4317 with the prefetch bit cleared and WS1's second bit kept
    // clear: 0x0317.
    gba.bus.write16(IO_BASE + WAITCNT as u32, 0x0317);
    assert!(!gba.bus.prefetch_enabled());
    assert_eq!(first_access(&mut gba, ROM_BASE, 2), 4, "WS0: 3 waits + 1");
    assert_eq!(
        first_access(&mut gba, ROM_WS1_BASE, 2),
        5,
        "WS1: 4 waits + 1"
    );
    assert_eq!(
        first_access(&mut gba, ROM_WS2_BASE, 2),
        9,
        "WS2: 8 waits + 1"
    );
    // And their sequential halves, which are what a 32-bit access pays second: WS0's second field
    // bit is set (1 wait), WS1's and WS2's are clear (4 and 8).
    gba.bus.write16(IO_BASE + WAITCNT as u32, 0x0017);
    assert_eq!(first_access(&mut gba, ROM_BASE, 4), 4 + 2, "WS0: 4 then 2");
    assert_eq!(
        first_access(&mut gba, ROM_WS1_BASE, 4),
        5 + 5,
        "WS1: 5 then 5"
    );
    assert_eq!(
        first_access(&mut gba, ROM_WS2_BASE, 4),
        5 + 9,
        "WS2: 5 then 9"
    );
}

#[test]
fn the_prefetch_buffer_makes_a_straight_run_cheaper_and_a_branch_refills_it() {
    // Eight ARM fetches, one after another, with the buffer on: the first pays the non-sequential
    // 4 + 2 and the next seven are buffer hits at one cycle for each of their two fragments.
    let mut gba = timing_machine();
    gba.bus.write16(IO_BASE + WAITCNT as u32, 0x4317); // prefetch on
    gba.bus.last_fetch_end = None;
    gba.bus.prefetch_next = None;
    let mut on = 0;
    for index in 0..8u32 {
        gba.bus.waits = 0;
        let _ = gba.bus.fetch32(ROM_BASE + index * 4);
        on += 1 + gba.bus.waits;
    }
    assert_eq!(on, 6 + 7 * 2, "prefetch on: {on} cycles for eight fetches");

    let mut gba = timing_machine();
    gba.bus.write16(IO_BASE + WAITCNT as u32, 0x0317); // prefetch off
    gba.bus.last_fetch_end = None;
    let mut before = 0;
    for index in 0..8u32 {
        gba.bus.waits = 0;
        let _ = gba.bus.fetch32(ROM_BASE + index * 4);
        before += 1 + gba.bus.waits;
    }
    // With the buffer off the first fetch is still 4 + 2, but each following one is 2 + 2.
    assert_eq!(
        before,
        6 + 7 * 4,
        "prefetch off: {before} cycles for eight fetches"
    );

    // A branch back to the top is not a buffer hit: the buffer is asked for an address it does not
    // hold, so it refills at the non-sequential cost.
    let mut gba = timing_machine();
    gba.bus.write16(IO_BASE + WAITCNT as u32, 0x4317);
    gba.bus.last_fetch_end = None;
    gba.bus.prefetch_next = None;
    let mut forward = 0;
    for index in 0..4u32 {
        gba.bus.waits = 0;
        let _ = gba.bus.fetch32(ROM_BASE + index * 4);
        forward += 1 + gba.bus.waits;
    }
    gba.bus.waits = 0;
    let _ = gba.bus.fetch32(ROM_BASE); // backwards
    let back = 1 + gba.bus.waits;
    assert_eq!(forward, 6 + 3 * 2, "the run while the buffer fills");
    assert_eq!(back, 6, "and the branch pays the non-sequential cost again");
}

#[test]
fn a_loop_of_rom_reads_is_cheaper_with_the_prefetch_buffer_on() {
    // The measurement the brief asks for, through the CPU: a loop that reads ROM in words, run once
    // with the buffer on and once with it off.
    fn run(prefetch: bool) -> u64 {
        // mov r4,#count / mov r1,#08000000 / .loop: ldr r0,[r1],#4 / subs r4,#1 / bne .loop / b .
        let program = [
            0xE3A04C01,  // mov r4,#0x100
            0xE3A0_1408, // mov r1,#$08000000
            0xE491_0004, // ldr r0,[r1],#4
            0xE254_4001, // subs r4,r4,#1
            0x1AFF_FFFC, // bne .loop
            0xEAFF_FFFE, // b .
        ];
        let mut gba = machine(&program);
        let waitcnt = if prefetch { 0x4317 } else { 0x0317 };
        gba.bus.write16(IO_BASE + WAITCNT as u32, waitcnt);
        let before = gba.bus.cycles;
        for _ in 0..1000 {
            gba.step_instruction();
            if gba.cpu.r[4] == 0 {
                break;
            }
        }
        gba.bus.cycles - before
    }
    let with = run(true);
    let without = run(false);
    // 256 reads of a word from ROM. The buffer turns each sequential fetch into a one-cycle access,
    // so the loop is measurably shorter — this is the claim, measured, not a feeling.
    assert!(
        with < without,
        "prefetch on cost {with} cycles, off cost {without}"
    );
    assert!(
        without - with > 200,
        "the difference should be about two cycles a read: {with} against {without}"
    );
    println!("a 256-iteration ROM read loop: {with} cycles with prefetch, {without} without");
}

#[test]
fn a_vblank_raises_if_when_dispstat_asks_for_it() {
    let mut gba = machine(&[0xEAFF_FFFE]);
    assert_eq!(gba.bus.read16(IO_BASE + 0x202), 0, "nothing pending");
    gba.bus.tick(CYCLES_PER_FRAME / 2); // not into V-Blank yet
    assert_eq!(gba.bus.read16(IO_BASE + 0x202), 0);
    // Enable the V-Blank interrupt: DISPSTAT bit 3.
    gba.bus.write16(IO_BASE + 0x004, 1 << 3);
    gba.bus.tick(CYCLES_PER_FRAME / 2 + 160 * 1232);
    assert_eq!(gba.bus.read16(IO_BASE + 0x202) & 1, 1, "IF bit 0 rose");
    // An acknowledge clears it, as a write of ones does.
    gba.bus.write16(IO_BASE + 0x202, 1);
    assert_eq!(gba.bus.read16(IO_BASE + 0x202) & 1, 0);
}

// ---------------------------------------------------------------- the video memories' byte rules

#[test]
fn a_byte_store_to_palette_or_bitmap_vram_is_written_twice() {
    let mut gba = machine(&[]);
    // Bitmap mode 4, as the public suite's text routine leaves `DISPCNT`.
    gba.bus.write16(IO_BASE, 4);
    gba.bus.write8(VRAM_BASE + 0x10, 2);
    assert_eq!(
        gba.bus.read16(VRAM_BASE + 0x10),
        0x0202,
        "memory.gba test 53"
    );
    gba.bus.write8(PALETTE_BASE + 0x20, 1);
    assert_eq!(
        gba.bus.read16(PALETTE_BASE + 0x20),
        0x0101,
        "memory.gba test 54"
    );
}

#[test]
fn a_byte_store_to_tiled_vram_or_to_oam_is_dropped() {
    let mut gba = machine(&[]);
    // Tiled mode 0.
    gba.bus.write16(IO_BASE, 0);
    gba.bus.write8(VRAM_BASE + 0x10 + 0x10000, 2);
    assert_eq!(
        gba.bus.read16(VRAM_BASE + 0x10 + 0x10000),
        0,
        "memory.gba test 52"
    );
    gba.bus.write8(OAM_BASE + 0x10, 1);
    assert_eq!(gba.bus.read16(OAM_BASE + 0x10), 0, "memory.gba test 50");
    // A halfword store to either of them works in any mode.
    gba.bus.write16(OAM_BASE + 0x10, 0x1234);
    assert_eq!(gba.bus.read16(OAM_BASE + 0x10), 0x1234);
    gba.bus.write16(VRAM_BASE + 0x10, 0x5678);
    assert_eq!(gba.bus.read16(VRAM_BASE + 0x10), 0x5678);
}

#[test]
fn vram_mirrors_the_way_each_bg_mode_lays_it_out() {
    let mut gba = machine(&[]);
    // Tiled: 64 KiB of BG data, then the object window, which repeats once at $18000.
    gba.bus.write16(IO_BASE, 0);
    gba.bus.write16(VRAM_BASE + 0x10000, 0x2222);
    assert_eq!(
        gba.bus.read16(VRAM_BASE + 0x18000),
        0x2222,
        "memory.gba test 5"
    );
    // Bitmap: the whole 96 KiB is linear and repeats at $20000.
    gba.bus.write16(IO_BASE, 3);
    gba.bus.write16(VRAM_BASE, 0x1111);
    assert_eq!(
        gba.bus.read16(VRAM_BASE + 0x20000),
        0x1111,
        "memory.gba test 4"
    );
    assert_ne!(
        gba.bus.read16(VRAM_BASE + 0x18000),
        0x2222,
        "and $18000 is a hole in a bitmap mode"
    );
}

// ---------------------------------------------------------------- the public gates

/// Run a suite for 600 frames the way G1's gate does, and report the machine.
fn run_suite(path: &str) -> Gba {
    let rom = std::fs::read(path)
        .unwrap_or_else(|why| panic!("{path}: {why} (scripts/fetch-test-roms.sh)"));
    let mut gba = Gba::new(Cartridge::new(rom).expect("a public test ROM"));
    for _ in 0..600 {
        gba.run_frame();
    }
    gba
}

/// The word at the program counter: the idle branch is `b .` — `$EAFFFFFE`.
const IDLE_BRANCH: u32 = 0xEAFF_FFFE;

#[test]
#[ignore = "needs roms/test/gba/bios.gba, a public test ROM kept out of the repository"]
fn bios_gba_reports_no_failed_tests() {
    let mut gba = run_suite(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/roms/test/gba/bios.gba"
    ));
    let parked = gba.bus.read32(gba.cpu.pc() & !3);
    println!(
        "bios.gba: {} frames, {} cycles, PC {:08X}, {} SWIs, calls {:?}, record {:?}",
        gba.frames(),
        gba.bus.cycles,
        gba.cpu.pc(),
        gba.cpu.swi_calls,
        [gba.bus.bios.calls_to(0x06), gba.bus.bios.calls_to(0x08),],
        gba.bus.bios.unimplemented
    );
    // The suite's four tests: the BIOS read after boot, after an SWI, during its own V-Blank
    // handler, and after it. Its *pass* path calls `Sqrt` once and prints; its failure path is the
    // only code that calls `Div` — twice, to print the test number.
    assert_eq!(gba.bus.bios.calls_to(0x06), 0, "the failure path ran");
    assert_eq!(gba.bus.bios.calls_to(0x08), 1, "the suite's own Sqrt");
    assert_eq!(gba.cpu.swi_calls, 1, "and nothing else called the BIOS");
    assert_eq!(gba.cpu.swi, None, "no call was left unserviced");
    assert_eq!(
        parked,
        IDLE_BRANCH,
        "the ROM reached its idle branch, PC {:08X}",
        gba.cpu.pc()
    );
    // Test 3's handler stored the BIOS read it made at $03000000; that word is the one a read during
    // an IRQ returns, so its presence proves the V-Blank handler ran through the BIOS handler.
    assert_eq!(
        gba.bus.read32(IWRAM_BASE),
        OPCODE_DURING_IRQ,
        "the suite's IRQ handler ran and saw [0134h+8]"
    );
    // The pass path draws "All tests passed" through the suite's glyph routine, which is a fistful
    // of `ldm`/`stm` stores into VRAM and two palette writes. Both are the marks that the printing
    // path ran rather than the ROM stopping early.
    let vram = gba.bus.vram.iter().filter(|&&byte| byte != 0).count();
    assert!(
        vram > 200,
        "the suite drew its result: {vram} VRAM bytes set"
    );
    assert_eq!(
        gba.bus.read16(PALETTE_BASE),
        0xFFFF,
        "colour 0 is white, written by m_test_init"
    );
    println!("bios.gba: {vram} VRAM bytes of glyph data");
}

#[test]
#[ignore = "needs roms/test/gba/memory.gba, a public test ROM kept out of the repository"]
fn memory_gba_reports_no_failed_tests() {
    let mut gba = run_suite(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/roms/test/gba/memory.gba"
    ));
    let parked = gba.bus.read32(gba.cpu.pc() & !3);
    println!(
        "memory.gba: {} frames, {} cycles, PC {:08X}, {} SWIs, record {:?}",
        gba.frames(),
        gba.bus.cycles,
        gba.cpu.pc(),
        gba.cpu.swi_calls,
        gba.bus.bios.unimplemented
    );
    // This suite calls no BIOS function on its pass path, and its failure path calls `Div` twice to
    // print the test number — so zero SWIs is the pass condition, judged G1's way.
    assert_eq!(gba.cpu.swi_calls, 0, "the failure path ran");
    assert_eq!(gba.cpu.swi, None, "no call was left unserviced");
    assert_eq!(
        parked,
        IDLE_BRANCH,
        "the ROM reached its idle branch, PC {:08X}",
        gba.cpu.pc()
    );
    // Eight mirror tests and five byte-store tests run before `eval`; the text routine's `stm`/`ldm`
    // marks are in VRAM, and the palette's three colours were written by `m_test_init`.
    let vram = gba.bus.vram.iter().filter(|&&byte| byte != 0).count();
    assert!(
        vram > 200,
        "the suite drew its result: {vram} VRAM bytes set"
    );
    assert_eq!(gba.bus.read16(PALETTE_BASE), 0xFFFF);
    assert_eq!(gba.bus.read16(PALETTE_BASE + 2), 0x0000);
    assert_eq!(gba.bus.read16(PALETTE_BASE + 4), 0xFFFF);
}

#[test]
#[ignore = "needs roms/test/gba/arm.gba and thumb.gba, public test ROMs kept out of the repository"]
fn the_earlier_suites_are_unmoved() {
    // G1's gates, run with G2's timing and BIOS in place: no failure-path SWI, the idle branch, and
    // the block-transfer marks G1's gate reads.
    for name in ["arm.gba", "thumb.gba"] {
        let path = format!("{}/roms/test/gba/{name}", env!("CARGO_MANIFEST_DIR"));
        let mut gba = run_suite(&path);
        let witness: Vec<u8> = (0..12)
            .map(|index| gba.bus.read8(IWRAM_BASE + index))
            .collect();
        let parked = gba.bus.read32(gba.cpu.pc() & !3);
        println!(
            "{name}: {} frames, {} cycles, PC {:08X}, {} SWIs, witness {witness:?}",
            gba.frames(),
            gba.bus.cycles,
            gba.cpu.pc(),
            gba.cpu.swi_calls
        );
        assert_eq!(gba.cpu.swi_calls, 0, "{name}: the failure path ran");
        assert_eq!(
            parked, IDLE_BRANCH,
            "{name}: the ROM reached its idle branch"
        );
        assert_eq!(gba.cpu.swi, None, "{name}: no call was left unserviced");
    }
}

#[test]
#[ignore = "needs a release build and the public suites, and measures rather than asserts"]
fn the_numbers_behind_the_report() {
    let gba = run_suite(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/roms/test/gba/memory.gba"
    ));
    println!(
        "memory.gba: {} instructions, {} cycles ({} an instruction), {} frames",
        gba.cpu.instructions,
        gba.bus.cycles,
        gba.bus.cycles as f64 / gba.cpu.instructions as f64,
        gba.frames()
    );
    assert_eq!(gba.frames(), 600);
}
