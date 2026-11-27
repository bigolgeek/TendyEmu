//! The four area commands, `ATTR_BLK` (`$04`), `ATTR_LIN` (`$05`), `ATTR_DIV` (`$06`)
//! and `ATTR_CHR` (`$07`), on the 20×18 live map.
//!
//! The layouts are Pan Docs, *Color Attribute Commands*, byte for byte:
//!
//! ```text
//! $04 ATTR_BLK  0 Command*8+Length (1..7)     $05 ATTR_LIN  0 Command*8+Length (1..7)
//!               1 Number of Data Sets                       1 Number of Data Sets ($01..$6E)
//!               2-7 Data Set #1                             2 Data Set #1
//!                   Control Code (bits 0/1/2 =                         Bit 0-4 Line Number
//!                   inside / line / outside;                            Bit 5-6 Palette
//!                   "When changing only the Inside or                   Bit 7   H/V (0=Vertical)
//!                   Outside, then the Surrounding
//!                   line becomes automatically
//!                   changed to same color")
//!                   Palette Designation (bits 0-1 /
//!                   2-3 / 4-5 for the same three)
//!                   X1, Y1, X2, Y2 of the surrounding rectangle
//!
//! $06 ATTR_DIV  0 Command*8+Length (fixed 1)  $07 ATTR_CHR  0 Command*8+Length (1..6)
//!               1 bits 0-1 below/right,                     1 Beginning X
//!                 2-3 above/left, 4-5 line,                 2 Beginning Y
//!                 bit 6 H/V (0=left/right)                  3-4 Number of Data Sets (1-360)
//!               2 X- or Y-coordinate                        5 Writing Style (0=L→R, 1=T→B)
//!                                                            6+ Data Sets, 1 in MSBs
//! ```
//!
//! **The table is the gate.** Every row compares all 90 bytes of the live map after the command,
//! so a command that paints the right cell and the wrong neighbour cannot pass. The expected maps
//! are built here from the geometry — [`on_line`], [`inside`], and explicit per-row conditions —
//! through [`packed`], a writer that ORs two bits into each cell rather than running the
//! production code's setter.
//!
//! Rows: `ATTR_BLK` inside only, line only, outside only, all three, control 0, clipped right,
//! clipped bottom, four data sets across two packets, replay, and palette 3; `ATTR_LIN`
//! horizontal and vertical; `ATTR_DIV` left/right and above/below; `ATTR_CHR` both writing
//! directions, its wrap at the right edge, and a short payload that must change nothing.

mod common;

use tendy_gb::sgb::{ATF_BYTES, CELLS_DOWN, CELLS_WIDE, Packet, Sgb};

/// Pack one palette per cell into a 90-byte map — Pan Docs' "two most significant bits of the first
/// byte ... the next two bits the next character", 20 cells a row, 18 rows.
fn packed(cells: impl Fn(usize, usize) -> u8) -> [u8; ATF_BYTES] {
    let mut map = [0u8; ATF_BYTES];
    for y in 0..CELLS_DOWN {
        for x in 0..CELLS_WIDE {
            let entry = y * CELLS_WIDE + x;
            map[entry / 4] |= (cells(x, y) & 0x03) << (6 - 2 * (entry % 4));
        }
    }
    map
}

/// A cell on the surrounding line of the rectangle `x1,y1 ..= x2,y2` — the four edges themselves.
fn on_line(x: usize, y: usize, x1: usize, y1: usize, x2: usize, y2: usize) -> bool {
    (x1..=x2).contains(&x) && (y1..=y2).contains(&y) && (x == x1 || x == x2 || y == y1 || y == y2)
}

/// A cell strictly inside the surrounding rectangle.
fn inside(x: usize, y: usize, x1: usize, y1: usize, x2: usize, y2: usize) -> bool {
    (x1 + 1..x2).contains(&x) && (y1 + 1..y2).contains(&y)
}

/// One command packet: `header = code << 3 | length`, then up to 15 data bytes, the rest zero —
/// Pan Docs: "Unused bytes at the end of the last packet should be set to zero."
fn cmd(code: u8, length: u8, data: &[u8]) -> Packet {
    assert!(data.len() <= 15, "a first packet carries 15 data bytes");
    let mut packet = Packet::default();
    packet.bytes[0] = code << 3 | length;
    packet.bytes[1..1 + data.len()].copy_from_slice(data);
    packet
}

/// A continuation packet: sixteen data bytes and no header, the way Pan Docs says a transmission's
/// second and later packets arrive.
fn continuation(data: &[u8]) -> Packet {
    assert!(data.len() <= 16, "a continuation carries 16 data bytes");
    let mut packet = Packet::default();
    packet.bytes[..data.len()].copy_from_slice(data);
    packet
}

/// A `Sgb` with SGB on and the named cells already painted.
fn sgb_with(cells: &[(usize, usize, u8)]) -> Sgb {
    let mut sgb = Sgb::on();
    for &(x, y, palette) in cells {
        sgb.attr_mut().set_cell(x, y, palette);
    }
    sgb
}

/// Every cell on `shift`, for the "nothing changed" rows.
fn pattern(x: usize, y: usize) -> u8 {
    ((x * 3 + y * 5 + 1) % 4) as u8
}

// ------------------------------------------------------------------ ATTR_BLK ($04)

/// The rectangle every `ATTR_BLK` row below paints, and Pan Docs' three palette fields:
/// inside 1, line 2, outside 3 (`0x39` = 00 11 10 01).
const RECT: (usize, usize, usize, usize) = (2, 2, 8, 5);
const COLOURS: u8 = 0x39;

fn blk(control: u8, rect: (u8, u8, u8, u8)) -> Packet {
    let (x1, y1, x2, y2) = rect;
    cmd(0x04, 1, &[1, control, COLOURS, x1, y1, x2, y2])
}

/// Inside only: the interior takes palette 1, and Pan Docs' exception gives the surrounding line
/// "the same color" — so the whole closed rectangle is palette 1 and the rest is untouched.
#[test]
fn attr_blk_inside_only_paints_the_line_too() {
    let mut sgb = Sgb::on();
    sgb.apply(&blk(0x01, (2, 2, 8, 5)), &[]);
    let (x1, y1, x2, y2) = RECT;
    let expected = packed(|x, y| {
        if (x1..=x2).contains(&x) && (y1..=y2).contains(&y) {
            1
        } else {
            0
        }
    });
    assert_eq!(sgb.attr().live(), &expected);
}

/// Line only: bit 1 set and nothing else — the four edges take palette 2, interior and outside stay
/// 0 even though their palette fields name 1 and 3.
#[test]
fn attr_blk_line_only_paints_the_edges() {
    let mut sgb = Sgb::on();
    sgb.apply(&blk(0x02, (2, 2, 8, 5)), &[]);
    let (x1, y1, x2, y2) = RECT;
    let expected = packed(|x, y| u8::from(on_line(x, y, x1, y1, x2, y2)) * 2);
    assert_eq!(sgb.attr().live(), &expected);
}

/// Outside only: everything outside the closed rectangle takes palette 3, the interior stays 0, and
/// the exception makes the line palette 3 too — the whole map is 3 except the 5×2 interior.
#[test]
fn attr_blk_outside_only_paints_the_world_and_the_line() {
    let mut sgb = Sgb::on();
    sgb.apply(&blk(0x04, (2, 2, 8, 5)), &[]);
    let (x1, y1, x2, y2) = RECT;
    let expected = packed(|x, y| if inside(x, y, x1, y1, x2, y2) { 0 } else { 3 });
    assert_eq!(sgb.attr().live(), &expected);
}

/// All three: 1 inside, 2 on the line, 3 outside — the exception must not fire when the line is
/// named explicitly, so the line keeps its own palette rather than the inside's or outside's.
#[test]
fn attr_blk_all_three_regions_keep_their_own_palettes() {
    let mut sgb = Sgb::on();
    sgb.apply(&blk(0x07, (2, 2, 8, 5)), &[]);
    let (x1, y1, x2, y2) = RECT;
    let expected = packed(|x, y| {
        if inside(x, y, x1, y1, x2, y2) {
            1
        } else if on_line(x, y, x1, y1, x2, y2) {
            2
        } else {
            3
        }
    });
    assert_eq!(sgb.attr().live(), &expected);
}

/// A control byte of 0 asks for nothing: the whole 90-byte map keeps the pattern it had.
#[test]
fn attr_blk_control_zero_changes_nothing() {
    let cells: Vec<(usize, usize, u8)> = (0..CELLS_DOWN)
        .flat_map(|y| (0..CELLS_WIDE).map(move |x| (x, y, pattern(x, y))))
        .collect();
    let mut sgb = sgb_with(&cells);
    assert_eq!(
        sgb.attr().live(),
        &packed(pattern),
        "the seed is the pattern"
    );

    sgb.apply(&blk(0x00, (2, 2, 8, 5)), &[]);
    assert_eq!(sgb.attr().live(), &packed(pattern));
}

/// Clipped at the right edge: `X2 = 25` is off the 20-wide grid, so the rectangle ends at 19 —
/// "clipped, not wrapped". The interior is 16..=18, the line is x=15/x=19 and y=1/y=4.
#[test]
fn attr_blk_clips_at_the_right_edge() {
    let mut sgb = Sgb::on();
    sgb.apply(&blk(0x07, (15, 1, 25, 4)), &[]);
    let expected = packed(|x, y| {
        if inside(x, y, 15, 1, 19, 4) {
            1
        } else if on_line(x, y, 15, 1, 19, 4) {
            2
        } else {
            3
        }
    });
    assert_eq!(sgb.attr().live(), &expected);
    assert_eq!(
        sgb.attr().palette_at(19, 1),
        2,
        "the clipped edge is a line"
    );
    assert_eq!(sgb.attr().palette_at(18, 2), 1, "the visible interior");
}

/// Clipped at the bottom edge: `Y2 = 25` stops at 17, and the interior loses the rows below it.
#[test]
fn attr_blk_clips_at_the_bottom_edge() {
    let mut sgb = Sgb::on();
    sgb.apply(&blk(0x07, (3, 14, 7, 25)), &[]);
    let expected = packed(|x, y| {
        if inside(x, y, 3, 14, 7, 17) {
            1
        } else if on_line(x, y, 3, 14, 7, 17) {
            2
        } else {
            3
        }
    });
    assert_eq!(sgb.attr().live(), &expected);
    assert_eq!(sgb.attr().palette_at(3, 17), 2, "the clipped bottom edge");
}

/// The four-data-set transfer, as the two packets Pan Docs says it arrives in: the first carries
/// the header, the count, data sets 1 and 2 and the first two bytes of data set 3; the second
/// carries the rest of set 3, all of set 4 and six zero pad bytes.
fn four_set_transfer() -> [Packet; 2] {
    let sets: [[u8; 6]; 4] = [
        // (control, colours, x1, y1, x2, y2)
        [0x02, 0x04, 2, 2, 10, 6],   // line only, palette 1
        [0x03, 0x0A, 12, 2, 18, 6],  // inside + line, both palette 2
        [0x02, 0x0C, 4, 8, 9, 12],   // line only, palette 3
        [0x03, 0x05, 6, 10, 14, 14], // inside + line, both palette 1
    ];
    let mut payload = vec![4u8];
    for set in sets {
        payload.extend_from_slice(&set);
    }
    assert_eq!(payload.len(), 25, "count + four six-byte sets");

    let mut first = Packet::default();
    first.bytes[0] = 0x04 << 3 | 2;
    first.bytes[1..16].copy_from_slice(&payload[..15]);
    let second = continuation(&payload[15..]);
    [first, second]
}

/// The map [`four_set_transfer`] must produce, set by set, later sets over earlier ones.
fn four_set_expected() -> [u8; ATF_BYTES] {
    packed(|x, y| {
        let mut palette = 0;
        if on_line(x, y, 2, 2, 10, 6) {
            palette = 1;
        }
        if (12..=18).contains(&x) && (2..=6).contains(&y) {
            palette = 2;
        }
        if on_line(x, y, 4, 8, 9, 12) {
            palette = 3;
        }
        if (6..=14).contains(&x) && (10..=14).contains(&y) {
            palette = 1;
        }
        palette
    })
}

/// Two packets, four data sets: sets 3 and 4 only exist in the second packet, and set 4 overlaps
/// set 3 where the later set must win. A reassembly that dropped the continuation, or applied the
/// sets out of order, cannot produce this map.
#[test]
fn attr_blk_reassembles_four_data_sets_from_two_packets() {
    let [first, second] = four_set_transfer();
    let mut sgb = Sgb::on();

    sgb.apply(&first, &[]);
    assert_eq!(
        sgb.attr().live(),
        &[0u8; ATF_BYTES],
        "half a transfer must not paint half the sets"
    );

    sgb.apply(&second, &[]);
    assert_eq!(sgb.attr().live(), &four_set_expected());
    assert_eq!(sgb.attr().palette_at(8, 11), 1, "set 4 over set 3");
    assert_eq!(
        sgb.attr().palette_at(4, 9),
        3,
        "set 3's line, outside set 4"
    );
}

/// The same two packets a second time must leave the map exactly where the first did: the commands
/// are absolute paints, not XORs.
#[test]
fn attr_blk_replayed_is_idempotent() {
    let [first, second] = four_set_transfer();
    let mut sgb = Sgb::on();
    for _ in 0..2 {
        sgb.apply(&first, &[]);
        sgb.apply(&second, &[]);
    }
    assert_eq!(sgb.attr().live(), &four_set_expected());
}

/// Palette 3 is 11b, and a writer that dropped the top of each two-bit field would paint 0 here:
/// four cells of `0xFF` are four cells of palette 3.
#[test]
fn attr_chr_keeps_the_top_two_bits_of_each_set() {
    let mut sgb = Sgb::on();
    sgb.apply(&cmd(0x07, 1, &[0, 0, 4, 0, 0, 0xFF]), &[]);
    let expected = packed(|x, y| if x < 4 && y == 0 { 3 } else { 0 });
    assert_eq!(sgb.attr().live(), &expected);
}

// ------------------------------------------------------------------ ATTR_LIN ($05)

/// Horizontal lines are rows: bit 7 set, bits 0-4 the row, bits 5-6 the palette. Row 3 palette 2
/// (`0xC3`), row 17 palette 3 (`0xF1`) — both run the full 20 cells.
#[test]
fn attr_lin_paints_whole_rows() {
    let mut sgb = Sgb::on();
    sgb.apply(&cmd(0x05, 1, &[2, 0xC3, 0xF1]), &[]);
    let expected = packed(|x, y| match y {
        3 => 2,
        17 => 3,
        _ => {
            let _ = x;
            0
        }
    });
    assert_eq!(sgb.attr().live(), &expected);
    assert_eq!(sgb.attr().palette_at(19, 3), 2, "the row reaches the edge");
}

/// Vertical lines are columns: bit 7 clear, bits 0-4 the column. Column 0 palette 1 (`0x20`),
/// column 19 palette 3 (`0x73`) — both run the full 18 rows.
#[test]
fn attr_lin_paints_whole_columns() {
    let mut sgb = Sgb::on();
    sgb.apply(&cmd(0x05, 1, &[2, 0x20, 0x73]), &[]);
    let expected = packed(|x, _y| match x {
        0 => 1,
        19 => 3,
        _ => 0,
    });
    assert_eq!(sgb.attr().live(), &expected);
    assert_eq!(
        sgb.attr().palette_at(19, 17),
        3,
        "the column reaches the bottom"
    );
}

// ------------------------------------------------------------------ ATTR_DIV ($06)

/// Split left/right at x=7: bits 2-3 (2) above/left, bits 0-1 (1) below/right, bits 4-5 (3) on the
/// division column. `0x39` has bit 6 clear, so the coordinate is an X.
#[test]
fn attr_div_splits_left_and_right() {
    let mut sgb = Sgb::on();
    sgb.apply(&cmd(0x06, 1, &[0x39, 7]), &[]);
    let expected = packed(|x, _y| match x.cmp(&7) {
        std::cmp::Ordering::Less => 2,
        std::cmp::Ordering::Equal => 3,
        std::cmp::Ordering::Greater => 1,
    });
    assert_eq!(sgb.attr().live(), &expected);
}

/// The same byte with bit 6 set splits above/below at y=9, and the coordinate is a Y.
#[test]
fn attr_div_splits_above_and_below() {
    let mut sgb = Sgb::on();
    sgb.apply(&cmd(0x06, 1, &[0x79, 9]), &[]);
    let expected = packed(|_x, y| match y.cmp(&9) {
        std::cmp::Ordering::Less => 2,
        std::cmp::Ordering::Equal => 3,
        std::cmp::Ordering::Greater => 1,
    });
    assert_eq!(sgb.attr().live(), &expected);
}

// ------------------------------------------------------------------ ATTR_CHR ($07)

/// Left to right from (2,3): ten sets, 1 2 3 0 1 2 3 0 1 2, four to a byte MSB first —
/// `0x6C` = 01 10 11 00, `0x60` = 01 10 00 00.
#[test]
fn attr_chr_writes_left_to_right() {
    let mut sgb = Sgb::on();
    sgb.apply(&cmd(0x07, 1, &[2, 3, 10, 0, 0, 0x6C, 0x6C, 0x60]), &[]);
    let run = [1u8, 2, 3, 0, 1, 2, 3, 0, 1, 2];
    let expected = packed(|x, y| {
        if y == 3 && (2..12).contains(&x) {
            run[x - 2]
        } else {
            0
        }
    });
    assert_eq!(sgb.attr().live(), &expected);
}

/// Top to bottom from (4,1): six sets down one column, 3 3 2 2 1 1 — `0xFA` = 11 11 10 10,
/// `0x50` = 01 01 00 00. Style bit 0 set, and its unused upper bits are zero here.
#[test]
fn attr_chr_writes_top_to_bottom() {
    let mut sgb = Sgb::on();
    sgb.apply(&cmd(0x07, 1, &[4, 1, 6, 0, 1, 0xFA, 0x50]), &[]);
    let run = [3u8, 3, 2, 2, 1, 1];
    let expected = packed(|x, y| {
        if x == 4 && (1..7).contains(&y) {
            run[y - 1]
        } else {
            0
        }
    });
    assert_eq!(sgb.attr().live(), &expected);
}

/// Pan Docs: "the function wraps to the next row/column when reaching the end of the screen".
/// Five sets from (18,2) run to (19,2) and finish at (0,3), (1,3), (2,3) with palettes 1 2 3 1 2 —
/// `0x6D` = 01 10 11 01, `0x80` = 10 00 00 00.
#[test]
fn attr_chr_wraps_at_the_right_edge() {
    let mut sgb = Sgb::on();
    sgb.apply(&cmd(0x07, 1, &[18, 2, 5, 0, 0, 0x6D, 0x80]), &[]);
    let expected = packed(|x, y| match (x, y) {
        (18, 2) => 1,
        (19, 2) => 2,
        (0, 3) => 3,
        (1, 3) => 1,
        (2, 3) => 2,
        _ => 0,
    });
    assert_eq!(sgb.attr().live(), &expected);
}

// ------------------------------------------------------------------ malformed payloads

/// A payload that announces more data than the transfer carries changes nothing at all — "never
/// half-apply". The map starts on a pattern and must end on exactly that pattern.
///
/// Pan Docs' padding rule matters here: the unused bytes of a packet "should be set to zero", so a
/// count that stays inside the 15 bytes of the first packet is a complete command whose tail reads
/// as palette-0 data sets — it is not short. Short means the count needs more bytes than the
/// transfer will ever deliver.
#[test]
fn short_payloads_change_nothing() {
    let cells: Vec<(usize, usize, u8)> = (0..CELLS_DOWN)
        .flat_map(|y| (0..CELLS_WIDE).map(move |x| (x, y, pattern(x, y))))
        .collect();
    let before = packed(pattern);

    // ATTR_BLK: three data sets are 18 bytes and the first packet carries 14 after the count.
    let mut sgb = sgb_with(&cells);
    sgb.apply(
        &cmd(
            0x04,
            1,
            &[3, 0x07, 0x39, 2, 2, 8, 5, 0x07, 0x39, 10, 2, 12, 6],
        ),
        &[],
    );
    assert_eq!(
        sgb.attr().live(),
        &before,
        "ATTR_BLK with a short third set"
    );

    // ATTR_BLK: no data set at all.
    let mut sgb = sgb_with(&cells);
    sgb.apply(&cmd(0x04, 1, &[2]), &[]);
    assert_eq!(sgb.attr().live(), &before, "ATTR_BLK with no sets");

    // ATTR_LIN: twenty lines need 21 bytes; one packet is 15.
    let mut sgb = sgb_with(&cells);
    sgb.apply(&cmd(0x05, 1, &[20, 0xC3, 0xF1]), &[]);
    assert_eq!(sgb.attr().live(), &before, "ATTR_LIN with a short list");

    // ATTR_CHR: sixty sets need seven data bytes, five header bytes before them.
    let mut sgb = sgb_with(&cells);
    sgb.apply(&cmd(0x07, 1, &[2, 3, 60, 0, 0, 0x6C]), &[]);
    assert_eq!(sgb.attr().live(), &before, "ATTR_CHR with a short run");

    // ATTR_CHR: zero sets is not a run, and more than the 360 cells is malformed.
    let mut sgb = sgb_with(&cells);
    sgb.apply(&cmd(0x07, 1, &[2, 3, 0, 0, 0, 0x6C]), &[]);
    assert_eq!(sgb.attr().live(), &before, "ATTR_CHR with a zero length");
    let mut sgb = sgb_with(&cells);
    sgb.apply(&cmd(0x07, 1, &[2, 3, 0x69, 0x01, 0, 0x6C]), &[]);
    assert_eq!(sgb.attr().live(), &before, "ATTR_CHR with 361 sets");

    // An ATTR_LIN that claims two packets and stops after the first: still nothing.
    let mut sgb = sgb_with(&cells);
    sgb.apply(&cmd(0x05, 2, &[20, 0xC3, 0xF1]), &[]);
    assert_eq!(
        sgb.attr().live(),
        &before,
        "a transfer that never completes is never applied"
    );
}

/// The area commands paint the live map only: the 45 uploaded files stay exactly as uploaded.
#[test]
fn the_area_commands_leave_the_files_alone() {
    let mut sgb = Sgb::on();
    sgb.attr_mut().attr_trn(&[0xAA; 4096]);
    sgb.apply(&blk(0x07, (2, 2, 8, 5)), &[]);
    assert_ne!(sgb.attr().live(), &[0u8; ATF_BYTES]);
    assert!(sgb.attr().files().iter().all(|&b| b == 0xAA));
}

// ------------------------------------------------------------------ through the machine

/// Drive `$FF00` the way a game does: one write per pulse, `0x10` a one bit, `0x20` a zero, and the
/// stop bit last.
fn bus_send(m: &mut tendy_gb::Machine, bytes: &[u8]) {
    m.bus.write(0xFF00, 0x00);
    for &byte in bytes {
        for bit in 0..8 {
            let one = (byte >> bit) & 1 == 1;
            m.bus.write(0xFF00, if one { 0x10 } else { 0x20 });
        }
    }
    m.bus.write(0xFF00, 0x20);
}

/// The whole path — `JOYP` receiver, `deliver`, `Sgb::apply`, the continuation guard, `areas` — for
/// the four-set transfer, and proof that the map is untouched until the second packet lands.
#[test]
fn the_receiver_hands_both_packets_to_the_area_commands() {
    let mut m = common::machine(common::synth_rom(&[]));
    m.bus.ppu.sgb = Sgb::on();

    let [first, second] = four_set_transfer();
    bus_send(&mut m, &first.bytes);
    assert_eq!(
        *m.bus.ppu.sgb.attr().live(),
        [0u8; ATF_BYTES],
        "the first packet alone paints nothing"
    );

    bus_send(&mut m, &second.bytes);
    assert_eq!(*m.bus.ppu.sgb.attr().live(), four_set_expected());
}

/// A state never carries a half-received transfer, so loading one must drop it. The
/// orphaned continuation must not be appended to nothing and complete a command from the old game.
#[test]
fn clearing_a_transfer_leaves_no_continuation_behind() {
    let [first, second] = four_set_transfer();
    let mut sgb = Sgb::on();
    sgb.apply(&first, &[]);
    assert!(sgb.receiving(), "the first of two packets is in flight");

    sgb.clear_transfer();
    assert!(!sgb.receiving(), "a loaded state starts with no transfer");

    sgb.apply(&second, &[]);
    assert_eq!(
        sgb.attr().live(),
        &[0u8; ATF_BYTES],
        "the orphaned continuation must not complete the old transfer"
    );
}

/// A continuation packet whose first data byte spells a `_TRN` header must still be taken as a
/// continuation. `$58 >> 3` decodes as `PAL_TRN` (`$0B`), `$98` as `CHR_TRN`, `$A0` as `PCT_TRN`
/// and `$A8` as `ATTR_TRN`, and area-command data can carry any of them, so the `_TRN` latch in
/// `Ppu::apply_sgb_packet` must not fire while an area transfer is in flight (`Sgb::receiving`).
#[test]
fn a_continuation_byte_that_spells_a_trn_header_is_still_data() {
    // The four magic first bytes and the `_TRN` command each one decodes as.
    for (first, trn) in [(0x58u8, 0x0B), (0x98, 0x13), (0xA0, 0x14), (0xA8, 0x15)] {
        assert_eq!(first >> 3, trn, "the byte must decode as a _TRN command");

        let mut m = common::machine(common::synth_rom(&[]));
        m.bus.ppu.sgb = Sgb::on();

        // An ATTR_BLK that expects one continuation packet: inside only, palette 1.
        bus_send(&mut m, &cmd(0x04, 2, &[1, 0x01, 0x39, 2, 2, 8, 5]).bytes);
        assert_eq!(
            *m.bus.ppu.sgb.attr().live(),
            [0u8; ATF_BYTES],
            "half a transfer paints nothing"
        );

        let mut second = Packet::default();
        second.bytes[0] = first;
        second.bytes[1] = 0xEE;
        bus_send(&mut m, &second.bytes);

        let (x1, y1, x2, y2) = RECT;
        let expected = packed(|x, y| {
            if (x1..=x2).contains(&x) && (y1..=y2).contains(&y) {
                1
            } else {
                0
            }
        });
        assert_eq!(
            *m.bus.ppu.sgb.attr().live(),
            expected,
            "a continuation starting ${first:02X} must complete the ATTR_BLK, not be latched as a _TRN"
        );
    }
}
