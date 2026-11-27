//! The expansion port: the 48-pin connector's extra controller lines, which a device there
//! drives onto `$4016`/`$4017` alongside whatever sits in the two NES ports (NESdev, "Expansion
//! port", https://www.nesdev.org/wiki/Expansion_port_pinout).
//!
//! The port carries `OUT0-OUT2` (the `$4016` write bits 0-2) to the device, and gives it `$4016`
//! D1 and `$4017` D1-D4 to drive on a read. The bus ORs those bits into the byte the front-loader
//! ports return, so an empty port changes nothing. Only [`ExpansionDevice::None`] ships with
//! the console; [`ExpansionDevice::Fixed`] is a stand-in that holds constant bits and records the
//! last OUT write, so the plumbing can be tested before a real device (Famicom keyboard, Family
//! Trainer, ...) needs it.
//!
//! Save-state layout (`STATE_VERSION` 7): one kind byte, a `u16` payload length, then the payload.
//! A later device takes a new kind byte and defines its own payload, so adding one needs no
//! version bump.

use crate::controller::{BUTTON_START, Paddle};
use crate::state::{StateError, StateReader, StateWriter};

/// Bits the expansion port can drive on a `$4016` read: D1.
pub const READ_4016_MASK: u8 = 0x02;
/// Bits the expansion port can drive on a `$4017` read: D1-D4.
pub const READ_4017_MASK: u8 = 0x1E;

const KIND_NONE: u8 = 0;
const KIND_FIXED: u8 = 1;
const KIND_PADDLE: u8 = 2;
/// `Paddle`'s payload: position, fire, shift.
const PADDLE_PAYLOAD: u16 = 3;
/// The Family BASIC keyboard. A new kind byte with its own payload length, which is
/// why `STATE_VERSION` does not move: an older build reads an unknown kind as an error and a state
/// saved with no device is still the three zero bytes.
const KIND_KEYBOARD: u8 = 3;
/// `FamilyKeyboard`'s payload: the last `$4016` write, the row, then one byte per key row.
const KEYBOARD_PAYLOAD: u16 = 2 + KEYBOARD_ROWS as u16;

/// Rows that carry keys. The matrix has a tenth, keyless row the scan counter passes through.
pub const KEYBOARD_ROWS: usize = 9;
/// Rows the scan counter walks before it wraps.
const SCAN_ROWS: u8 = 10;

/// The 72 keys by name, in matrix order: nine rows of eight, each row's first four entries being
/// column 0 and the last four column 1, each group listed `$4017` D4, D3, D2, D1. A key's id is
/// `row * 8 + position`. Names are what `keys.txt` writes after `keyboard.`.
///
/// Source: NESdev wiki, "Family BASIC Keyboard" (https://www.nesdev.org/wiki/Family_BASIC_Keyboard),
/// the Matrix table.
pub const FAMILY_KEY_NAMES: [&str; KEYBOARD_ROWS * 8] = [
    "rbracket",
    "lbracket",
    "return",
    "f8",
    "stop",
    "yen",
    "rshift",
    "kana",
    "semicolon",
    "colon",
    "at",
    "f7",
    "caret",
    "minus",
    "slash",
    "underscore",
    "k",
    "l",
    "o",
    "f6",
    "0",
    "p",
    "comma",
    "period",
    "j",
    "u",
    "i",
    "f5",
    "8",
    "9",
    "n",
    "m",
    "h",
    "g",
    "y",
    "f4",
    "6",
    "7",
    "v",
    "b",
    "d",
    "r",
    "t",
    "f3",
    "4",
    "5",
    "c",
    "f",
    "a",
    "s",
    "w",
    "f2",
    "3",
    "e",
    "z",
    "x",
    "ctr",
    "q",
    "esc",
    "f1",
    "2",
    "1",
    "grph",
    "lshift",
    "left",
    "right",
    "up",
    "clrhome",
    "ins",
    "del",
    "space",
    "down",
];

/// The Family BASIC keyboard (HVC-007), NESdev "Family BASIC Keyboard".
///
/// `$4016` write, `xxxx xKCR`: `R` (bit 0) resets the scan to row 0; `K` (bit 1) selects the
/// column, and the row advances when `K` falls from high to low; `E` (bit 2) enables the matrix,
/// and with it clear every `$4017` line reads 0. A read returns the selected row and column's four
/// keys on `$4017` D4-D1 (bits 4-1), a held key reading 0. The scan wraps after a tenth, keyless
/// row, which therefore reads `$1E` when enabled (the keyboard-detection trick Lode Runner uses).
/// `$4016` reads belong to the data recorder, which this does not model.
///
/// The nesdev procedure writes `$05` (reset to row 0), `$04` (column 0; no row step, since `K`
/// did not fall), reads, `$06` (column 1), reads, then `$04` again (`K` falls: next row).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FamilyKeyboard {
    /// Bit `position` of entry `row` is set while that key is down (see [`FAMILY_KEY_NAMES`]).
    pub held: [u8; KEYBOARD_ROWS],
    /// The row the scan counter is on, `0..10`.
    row: u8,
    /// The last `$4016` write's low three bits.
    ctrl: u8,
}

impl FamilyKeyboard {
    /// A `$4016` write.
    pub fn write(&mut self, val: u8) {
        let val = val & 0x07;
        if val & 0x01 != 0 {
            self.row = 0;
        } else if self.ctrl & 0x02 != 0 && val & 0x02 == 0 {
            self.row = (self.row + 1) % SCAN_ROWS;
        }
        self.ctrl = val;
    }

    /// The `$4017` D4-D1 lines (bits 4-1) for the current row and column.
    pub fn peek(&self) -> u8 {
        if self.ctrl & 0x04 == 0 {
            return 0;
        }
        let held = self.held.get(usize::from(self.row)).copied().unwrap_or(0);
        let column = (if self.ctrl & 0x02 != 0 {
            held >> 4
        } else {
            held
        }) & 0x0F;
        // Position 0 of a column is D4, position 3 is D1; a held key reads 0.
        let mut out = 0;
        for position in 0..4 {
            if column & (1 << position) == 0 {
                out |= 1 << (4 - position);
            }
        }
        out
    }

    /// The row the scan counter is on (tests and the front end's diagnostics).
    pub fn row(&self) -> u8 {
        self.row
    }

    fn save_state(&self, w: &mut StateWriter) {
        w.u8(self.ctrl);
        w.u8(self.row);
        for byte in self.held {
            w.u8(byte);
        }
    }

    fn load_state(&mut self, r: &mut StateReader) -> Result<(), StateError> {
        let ctrl = r.u8()?;
        let row = r.u8()?;
        if ctrl > 0x07 || row >= SCAN_ROWS {
            return Err(StateError::BadValue);
        }
        self.ctrl = ctrl;
        self.row = row;
        for byte in &mut self.held {
            *byte = r.u8()?;
        }
        Ok(())
    }
}

/// What is plugged into the expansion port.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum ExpansionDevice {
    /// Nothing: the port floats and contributes no bits.
    #[default]
    None,
    /// A test device: drives constant bits and remembers the last OUT value written.
    Fixed {
        /// Driven on `$4016` D1 (only bit 1 is used).
        d1_4016: u8,
        /// Driven on `$4017` D1-D4 (bits 1-4 are used).
        d4_4017: u8,
        /// The last `$4016` write's OUT0-OUT2 (bits 0-2).
        out: u8,
    },
    /// The Famicom Arkanoid (Vaus) controller, hard-wired to the expansion port (NESdev,
    /// "Arkanoid controller"). It is the same 4021 shift register the NES paddle uses, but it
    /// answers on different pins: the fire button on `$4016` D1 and the knob's inverted serial
    /// bit on `$4017` D1. A `$4016` write with OUT0 high latches the knob; each `$4017` read
    /// shifts one bit out.
    Paddle(Paddle),
    /// The Family BASIC keyboard: 72 keys scanned through `$4016` writes and read back on `$4017`.
    Keyboard(FamilyKeyboard),
}

impl ExpansionDevice {
    /// Bits driven onto a `$4016` read (D1 only). Clocks the device if it has a shift register.
    pub fn read_4016_d1(&mut self) -> u8 {
        self.peek_4016_d1()
    }

    /// Bits driven onto a `$4017` read (D1-D4). Clocks the device if it has a shift register.
    pub fn read_4017_d1_d4(&mut self) -> u8 {
        let byte = self.peek_4017_d1_d4();
        if let ExpansionDevice::Paddle(p) = self {
            p.read(); // clock the shift register once per /OE pulse
        }
        byte
    }

    /// `read_4016_d1` without clocking: what a read on a still-asserted /OE line returns.
    pub fn peek_4016_d1(&self) -> u8 {
        match self {
            ExpansionDevice::None => 0,
            ExpansionDevice::Fixed { d1_4016, .. } => d1_4016 & READ_4016_MASK,
            ExpansionDevice::Paddle(p) => {
                // `Paddle::peek` reports fire on its /D3 bit; the Famicom wires it to D1.
                if p.peek() & BUTTON_START != 0 {
                    0x02
                } else {
                    0
                }
            }
            // `$4016` D1 is the data recorder's input, which is not modelled.
            ExpansionDevice::Keyboard(_) => 0,
        }
    }

    /// `read_4017_d1_d4` without clocking.
    pub fn peek_4017_d1_d4(&self) -> u8 {
        match self {
            ExpansionDevice::None => 0,
            ExpansionDevice::Fixed { d4_4017, .. } => d4_4017 & READ_4017_MASK,
            // `Paddle::peek` reports the data bit on /D4 (bit 4); the Famicom wires it to D1.
            ExpansionDevice::Paddle(p) => (p.peek() >> 3) & 0x02,
            ExpansionDevice::Keyboard(k) => k.peek() & READ_4017_MASK,
        }
    }

    /// Replace the Family keyboard's held keys (one bit per key, see [`FAMILY_KEY_NAMES`]).
    /// Does nothing for any other device.
    pub fn set_keyboard(&mut self, held: [u8; KEYBOARD_ROWS]) {
        if let ExpansionDevice::Keyboard(k) = self {
            k.held = held;
        }
    }

    /// The Family keyboard, when that is what is plugged in.
    pub fn keyboard(&self) -> Option<&FamilyKeyboard> {
        match self {
            ExpansionDevice::Keyboard(k) => Some(k),
            _ => None,
        }
    }

    /// Move the Famicom paddle's knob to `position` (`0x00` left .. `0xFF` right) and set its
    /// fire button. Does nothing for any other device.
    pub fn set_paddle(&mut self, position: u8, fire: bool) {
        if let ExpansionDevice::Paddle(p) = self {
            p.position = position;
            p.fire = fire;
        }
    }

    /// The Famicom paddle, when that is what is plugged in.
    pub fn paddle(&self) -> Option<&Paddle> {
        match self {
            ExpansionDevice::Paddle(p) => Some(p),
            _ => None,
        }
    }

    /// A `$4016` write: the device sees OUT0-OUT2 (bits 0-2 of `val`).
    pub fn write_out(&mut self, val: u8) {
        if let ExpansionDevice::Fixed { out, .. } = self {
            *out = val & 0x07;
        } else if let ExpansionDevice::Paddle(p) = self {
            p.write(val & 0x01); // OUT0 is the strobe
        } else if let ExpansionDevice::Keyboard(k) = self {
            k.write(val);
        }
    }

    /// Payload size in bytes for this device's kind.
    fn payload_len(&self) -> u16 {
        match self {
            ExpansionDevice::None => 0,
            ExpansionDevice::Fixed { .. } => 3,
            ExpansionDevice::Paddle(_) => PADDLE_PAYLOAD,
            ExpansionDevice::Keyboard(_) => KEYBOARD_PAYLOAD,
        }
    }

    pub fn save_state(&self, w: &mut StateWriter) {
        match self {
            ExpansionDevice::None => w.u8(KIND_NONE),
            ExpansionDevice::Fixed { .. } => w.u8(KIND_FIXED),
            ExpansionDevice::Paddle(_) => w.u8(KIND_PADDLE),
            ExpansionDevice::Keyboard(_) => w.u8(KIND_KEYBOARD),
        }
        w.u16(self.payload_len());
        if let ExpansionDevice::Keyboard(k) = self {
            k.save_state(w);
        }
        if let ExpansionDevice::Fixed {
            d1_4016,
            d4_4017,
            out,
        } = self
        {
            w.u8(*d1_4016);
            w.u8(*d4_4017);
            w.u8(*out);
        }
        if let ExpansionDevice::Paddle(p) = self {
            p.save_state(w);
        }
    }

    /// Mirror of `save_state`. An unknown kind or a payload of the wrong size is an error rather
    /// than a silent skip: the state came from a build with a device this one cannot model.
    pub fn load_state(&mut self, r: &mut StateReader) -> Result<(), StateError> {
        let kind = r.u8()?;
        let len = r.u16()?;
        let expected = match kind {
            KIND_NONE => 0,
            KIND_FIXED => 3,
            KIND_PADDLE => usize::from(PADDLE_PAYLOAD),
            KIND_KEYBOARD => usize::from(KEYBOARD_PAYLOAD),
            _ => return Err(StateError::BadValue),
        };
        if usize::from(len) != expected {
            return Err(StateError::LengthMismatch {
                expected,
                found: usize::from(len),
            });
        }
        *self = match kind {
            KIND_FIXED => ExpansionDevice::Fixed {
                d1_4016: r.u8()?,
                d4_4017: r.u8()?,
                out: r.u8()?,
            },
            KIND_PADDLE => {
                let mut p = Paddle::default();
                p.load_state(r)?;
                ExpansionDevice::Paddle(p)
            }
            KIND_KEYBOARD => {
                let mut k = FamilyKeyboard::default();
                k.load_state(r)?;
                ExpansionDevice::Keyboard(k)
            }
            _ => ExpansionDevice::None,
        };
        Ok(())
    }
}
