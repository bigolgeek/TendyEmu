//! The four area commands — `ATTR_BLK` (`$04`), `ATTR_LIN` (`$05`), `ATTR_DIV` (`$06`) and
//! `ATTR_CHR` (`$07`) — applied to the live 20×18 palette map.
//!
//! Pan Docs, *Color Attribute Commands*, is the whole specification; the four byte tables below are
//! quoted from it verbatim so this leg never works from memory:
//!
//! ```text
//! $04 ATTR_BLK                            $05 ATTR_LIN
//!  0     Command*8+Length (length=1..7)    0     Command*8+Length (length=1..7)
//!  1     Number of Data Sets ($01..$12)    1     Number of Data Sets ($01..$6E)
//!  2-7   Data Set #1                       2     Data Set #1
//!          Byte 0 - Control Code (0-7)              Bit 0-4 - Line Number
//!            Bit 0 - Change Colors inside            Bit 5-6 - Palette Number (0-3)
//!            Bit 1 - Change surrounding line         Bit 7   - H/V (0=Vertical, 1=Horizontal)
//!            Bit 2 - Change Colors outside         3     Data Set #2 (if any)
//!            Bit 3-7 - Not used (zero)             ...
//!            Exception: When changing only the
//!            Inside or Outside, then the
//!            Surrounding line becomes
//!            automatically changed to same color.
//!          Byte 1 - Color Palette Designation
//!            Bit 0-1 - Palette inside
//!            Bit 2-3 - Palette surrounding line
//!            Bit 4-5 - Palette outside
//!            Bit 6-7 - Not used (zero)
//!          Byte 2 - X1 (left)   Byte 3 - Y1 (upper)
//!          Byte 4 - X2 (right)  Byte 5 - Y2 (lower)
//!
//! $06 ATTR_DIV (fixed length=1)           $07 ATTR_CHR (length=1..6)
//!  0     Command*8+Length                  0     Command*8+Length
//!  1     Palettes and H/V Mode             1     Beginning X-Coordinate
//!          Bit 0-1 - Palette below/right   2     Beginning Y-Coordinate
//!          Bit 2-3 - Palette above/left    3-4   Number of Data Sets (1-360)
//!          Bit 4-5 - Palette of the line   5     Writing Style (0=L→R, 1=T→B)
//!          Bit 6   - H/V (0=left/right,    6     Data Sets 1-4 (1 in MSBs, 4 in LSBs)
//!                            1=above/below) 7     Data Sets 5-8 (if any)
//!  2     X- or Y-Coordinate                ...
//!  3-F   Not used (zero)
//! ```
//!
//! **What they write.** Only the live map. The 45 attribute files belong to `ATTR_TRN` and the
//! palette store to `PAL_*`; an area command is a paint on the screen, not a file upload.
//!
//! **Multi-packet transfers.** Pan Docs: "a `Length` indicates the total number of packets (1-7,
//! including the first packet)", the first packet holds the header byte plus 15 data bytes, and
//! every later packet is 16 data bytes with no header of its own — `ATTR_BLK` with three or more
//! data sets, `ATTR_LIN` with fifteen or more and `ATTR_CHR` with forty-one or more continue into
//! further packets. A continuation packet therefore cannot be decoded as a command at all, so the
//! reassembly lives here: [`Areas`] keeps the command code, the bytes received so far and the count
//! of packets still to come, and applies the command in one go when the last packet lands. A
//! transfer that stops short applies nothing, so the map never shows half of a rectangle list.

use crate::sgb::attr::{AttrMap, CELLS_DOWN, CELLS_WIDE};
use crate::sgb::packets::Packet;

/// The four command codes, as `Packet::command()` reports them (`header >> 3`).
const BLK: u8 = 0x04;
const LIN: u8 = 0x05;
const DIV: u8 = 0x06;
const CHR: u8 = 0x07;

/// Bytes one transfer can carry: 15 in the first packet plus six × 16 continuation bytes — Pan
/// Docs' maximum length of seven packets. The longest payload a command can ask for is `ATTR_LIN`'s
/// 110 one-byte data sets plus its count, 111 bytes.
pub const MAX_PAYLOAD: usize = 15 + 6 * 16;

/// The multi-packet buffer and the four area commands.
///
/// Session state, not save-state, exactly like [`crate::sgb::packets::Responder`]'s packet in
/// flight: a snapshot is only written between frames, and a transfer still in flight when it is
/// written is one the game finishes before its own next snapshot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Areas {
    /// The command being reassembled, or `None` when no transfer is in flight.
    command: Option<u8>,
    /// Packets still expected: `length - 1` of the header's count.
    remaining: u8,
    /// The data bytes received so far, payload (byte 1 of the first packet) first.
    buffer: [u8; MAX_PAYLOAD],
    /// How much of `buffer` is live.
    len: usize,
}

impl Areas {
    /// No transfer in flight, every cell untouched.
    pub fn new() -> Areas {
        Areas {
            command: None,
            remaining: 0,
            buffer: [0; MAX_PAYLOAD],
            len: 0,
        }
    }

    /// True while a multi-packet transfer still expects continuation packets.
    ///
    /// `Sgb::apply` asks this before it decodes a header: a continuation packet's first byte is
    /// data, and data that happens to spell a command code must not be dispatched as one.
    pub fn receiving(&self) -> bool {
        self.remaining > 0
    }

    /// Take one packet: a continuation if a transfer is in flight, otherwise one of the four area
    /// commands (anything else is ignored).
    pub fn apply(&mut self, packet: &Packet, attr: &mut AttrMap) {
        if self.remaining > 0 {
            self.extend(&packet.bytes, attr);
            return;
        }
        let command = packet.command();
        if !matches!(command, BLK | LIN | DIV | CHR) {
            return;
        }
        // Pan Docs fixes `ATTR_DIV` at one packet whatever the header says; the other three carry
        // their total in the header and must not read a zero as "no packets".
        let packets = if command == DIV {
            1
        } else {
            packet.length().max(1)
        };
        self.command = Some(command);
        self.len = 15;
        self.buffer[..15].copy_from_slice(&packet.bytes[1..]);
        self.remaining = packets - 1;
        if self.remaining == 0 {
            self.finish(attr);
        }
    }

    /// Append one continuation packet's sixteen data bytes; apply the command when the last lands.
    fn extend(&mut self, bytes: &[u8; 16], attr: &mut AttrMap) {
        let room = (MAX_PAYLOAD - self.len).min(bytes.len());
        self.buffer[self.len..self.len + room].copy_from_slice(&bytes[..room]);
        self.len += room;
        self.remaining -= 1;
        if self.remaining == 0 {
            self.finish(attr);
        }
    }

    /// Apply the reassembled payload in one go, then go idle.
    fn finish(&mut self, attr: &mut AttrMap) {
        let command = self.command.take();
        let len = self.len;
        self.remaining = 0;
        let buffer = &self.buffer;
        let payload = &buffer[..len];
        match command {
            Some(BLK) => attr_blk(attr, payload),
            Some(LIN) => attr_lin(attr, payload),
            Some(DIV) => attr_div(attr, payload),
            Some(CHR) => attr_chr(attr, payload),
            _ => {}
        }
    }
}

impl Default for Areas {
    fn default() -> Self {
        Areas::new()
    }
}

/// `ATTR_BLK` (`$04`): every data set, in order, over the payload's `count`.
///
/// A data set is six bytes: control, palette designation, then X1, Y1, X2, Y2 "of the surrounding
/// rectangle". The rectangle's own edge is the "surrounding character line"; its interior is
/// everything strictly inside; everything else is outside. The three regions are disjoint, so the
/// order the three paints happen in cannot matter — the data sets' order does, and is kept: a later
/// set paints over an earlier one.
fn attr_blk(attr: &mut AttrMap, payload: &[u8]) {
    let Some((&count, mut rest)) = payload.split_first() else {
        return;
    };
    let count = usize::from(count);
    // All-or-nothing: a transfer whose last data set is short of six bytes applies nothing.
    if rest.len() < count * 6 {
        return;
    }
    for _ in 0..count {
        let set = &rest[..6];
        rest = &rest[6..];

        let control = set[0] & 0x07;
        let colours = set[1];
        let inside_palette = colours & 0x03;
        let line_palette = (colours >> 2) & 0x03;
        let outside_palette = (colours >> 4) & 0x03;

        // Coordinates are the window's 20×18 grid. A rectangle reaching past the edge is clipped
        // to it; one entirely past the edge is nothing at all. Never wrapped.
        let x1 = usize::from(set[2]);
        let y1 = usize::from(set[3]);
        let x2 = usize::from(set[4]).min(CELLS_WIDE - 1);
        let y2 = usize::from(set[5]).min(CELLS_DOWN - 1);
        if x1 > x2 || y1 > y2 || x1 >= CELLS_WIDE || y1 >= CELLS_DOWN {
            continue;
        }

        let change_inside = control & 0x01 != 0;
        let change_line = control & 0x02 != 0;
        let change_outside = control & 0x04 != 0;

        // Pan Docs: "When changing only the Inside or Outside, then the Surrounding line becomes
        // automatically changed to same color." A set that names no region changes nothing.
        let line = if change_line {
            Some(line_palette)
        } else if change_inside && !change_outside {
            Some(inside_palette)
        } else if change_outside && !change_inside {
            Some(outside_palette)
        } else {
            None
        };

        for y in 0..CELLS_DOWN {
            for x in 0..CELLS_WIDE {
                let in_rect = (x1..=x2).contains(&x) && (y1..=y2).contains(&y);
                if change_inside && (x1 + 1..x2).contains(&x) && (y1 + 1..y2).contains(&y) {
                    attr.set_cell(x, y, inside_palette);
                }
                if let Some(palette) = line
                    && in_rect
                    && (x == x1 || x == x2 || y == y1 || y == y2)
                {
                    attr.set_cell(x, y, palette);
                }
                if change_outside && !in_rect {
                    attr.set_cell(x, y, outside_palette);
                }
            }
        }
    }
}

/// `ATTR_LIN` (`$05`): whole lines, one byte per data set, in order.
///
/// Bits 0-5 are the line's coordinate (`0-31`, out of range means nothing on this screen), bits 5-6
/// its palette and bit 7 picks a horizontal line (a row, `y = number`) from a vertical one (a
/// column, `x = number`); each reaches "from one end of the screen to the other end". Pan Docs:
/// "lines from lastmost data sets will overwrite lines from previous data sets" — painting in
/// order gives exactly that.
fn attr_lin(attr: &mut AttrMap, payload: &[u8]) {
    let Some((&count, sets)) = payload.split_first() else {
        return;
    };
    let count = usize::from(count);
    if sets.len() < count {
        return;
    }
    for &set in &sets[..count] {
        let line = usize::from(set & 0x1F);
        let palette = (set >> 5) & 0x03;
        if set & 0x80 != 0 {
            // Horizontal: a whole row.
            if line < CELLS_DOWN {
                for x in 0..CELLS_WIDE {
                    attr.set_cell(x, line, palette);
                }
            }
        } else if line < CELLS_WIDE {
            // Vertical: a whole column.
            for y in 0..CELLS_DOWN {
                attr.set_cell(line, y, palette);
            }
        }
    }
}

/// `ATTR_DIV` (`$06`): the screen split in two by one line, "and to assign separate color
/// attributes to each half, and to the division line between them".
///
/// Bit 6 picks the split: clear is left/right (the division line is a column at the coordinate,
/// bits 2-3 left of it and bits 0-1 right of it), set is above/below (a row, bits 2-3 above, bits
/// 0-1 below). Bits 4-5 are the division line's own palette. A coordinate past the edge puts the
/// whole screen on the near side and shows no line.
fn attr_div(attr: &mut AttrMap, payload: &[u8]) {
    if payload.len() < 2 {
        return;
    }
    let colours = payload[0] & 0x7F;
    let coordinate = usize::from(payload[1]);
    let below_right = colours & 0x03;
    let above_left = (colours >> 2) & 0x03;
    let line = (colours >> 4) & 0x03;
    let split_rows = colours & 0x40 != 0;
    for y in 0..CELLS_DOWN {
        for x in 0..CELLS_WIDE {
            let axis = if split_rows { y } else { x };
            let palette = match axis.cmp(&coordinate) {
                std::cmp::Ordering::Less => above_left,
                std::cmp::Ordering::Equal => line,
                std::cmp::Ordering::Greater => below_right,
            };
            attr.set_cell(x, y, palette);
        }
    }
}

/// `ATTR_CHR` (`$07`): a run of cells, two bits each, in one of two directions.
///
/// Bytes 3-4 are the run's length, low byte first; byte 5 is the writing style (bit 0 clear writes
/// left to right, set writes top to bottom) and bits 1-7 are unused and masked. The data are "Set 1
/// in MSBs, Set 4 in LSBs" starting at byte 6. Pan Docs: "In either case the function wraps to the
/// next row/column when reaching the end of the screen".
fn attr_chr(attr: &mut AttrMap, payload: &[u8]) {
    if payload.len() < 5 {
        return;
    }
    let start_x = usize::from(payload[0]);
    let start_y = usize::from(payload[1]);
    let count = usize::from(payload[2]) | usize::from(payload[3]) << 8;
    let style = payload[4];
    // Pan Docs: "Number of Data Sets (1-360)" — one per cell, so more than the screen is malformed.
    if count == 0 || count > CELLS_WIDE * CELLS_DOWN {
        return;
    }
    // 5 header bytes and four two-bit sets per byte: a short payload changes nothing.
    if payload.len() < 5 + count.div_ceil(4) {
        return;
    }
    if start_x >= CELLS_WIDE || start_y >= CELLS_DOWN {
        return;
    }
    let mut x = start_x;
    let mut y = start_y;
    for index in 0..count {
        let byte = payload[5 + index / 4];
        let palette = (byte >> (6 - 2 * (index % 4))) & 0x03;
        attr.set_cell(x, y, palette);
        if style & 0x01 == 0 {
            x += 1;
            if x == CELLS_WIDE {
                x = 0;
                y = (y + 1) % CELLS_DOWN;
            }
        } else {
            y += 1;
            if y == CELLS_DOWN {
                y = 0;
                x = (x + 1) % CELLS_WIDE;
            }
        }
    }
}
