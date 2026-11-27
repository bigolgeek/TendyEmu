//! `$FF00` select lines and the 8-button pad.

use crate::bus::IF_JOYPAD;
use crate::state::{StateError, StateReader, StateWriter};

/// bit 0 Right, 1 Left, 2 Up, 3 Down, 4 A, 5 B, 6 Select, 7 Start.
///
/// The low nibble order is the `$FF00` matrix order, so reading P1 is a mask, not a shuffle.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Buttons(pub u8);

impl Buttons {
    pub const RIGHT: Buttons = Buttons(1 << 0);
    pub const LEFT: Buttons = Buttons(1 << 1);
    pub const UP: Buttons = Buttons(1 << 2);
    pub const DOWN: Buttons = Buttons(1 << 3);
    pub const A: Buttons = Buttons(1 << 4);
    pub const B: Buttons = Buttons(1 << 5);
    pub const SELECT: Buttons = Buttons(1 << 6);
    pub const START: Buttons = Buttons(1 << 7);
    pub const NONE: Buttons = Buttons(0);
}

impl std::ops::BitOr for Buttons {
    type Output = Buttons;

    fn bitor(self, rhs: Buttons) -> Buttons {
        Buttons(self.0 | rhs.0)
    }
}

impl std::ops::BitOrAssign for Buttons {
    fn bitor_assign(&mut self, rhs: Buttons) {
        self.0 |= rhs.0;
    }
}

/// The two shoulder buttons a Game Boy Advance has and a Game Boy does not (G7).
///
/// Kept beside [`Buttons`] instead of widening it: `Buttons` is one byte laid out as the Game Boy's
/// `$FF00` matrix and every Game Boy reader — [`Joypad`] above all — depends on that width. The
/// Advance's extra pair travels alongside and reaches `KEYINPUT` bits 8 (R) and 9 (L).
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Shoulders(pub u8);

impl Shoulders {
    /// `KEYINPUT` bit 9.
    pub const LEFT: Shoulders = Shoulders(1 << 0);
    /// `KEYINPUT` bit 8.
    pub const RIGHT: Shoulders = Shoulders(1 << 1);
    /// Neither shoulder held.
    pub const NONE: Shoulders = Shoulders(0);

    /// True when neither shoulder is held.
    pub fn is_empty(self) -> bool {
        self.0 == 0
    }
}

impl std::ops::BitOr for Shoulders {
    type Output = Shoulders;

    fn bitor(self, rhs: Shoulders) -> Shoulders {
        Shoulders(self.0 | rhs.0)
    }
}

impl std::ops::BitOrAssign for Shoulders {
    fn bitor_assign(&mut self, rhs: Shoulders) {
        self.0 |= rhs.0;
    }
}

/// The joypad register and the pressed matrix.
#[derive(Clone)]
pub struct Joypad {
    /// 1 = held down.
    pub pressed: Buttons,
    /// Bits 4-5 as written, active low.
    pub select: u8,
    /// Previous state of bits 0-3 after selection, for the edge.
    pub last_low: u8,
}

impl Joypad {
    /// Post-boot P1 = `$CF`: both rows selected, nothing pressed.
    pub fn new() -> Joypad {
        Joypad {
            pressed: Buttons::NONE,
            select: 0x00,
            last_low: 0x0F,
        }
    }

    pub fn read(&self) -> u8 {
        0xC0 | (self.select & 0x30) | self.low_nibble()
    }

    /// Stores bits 5-4; returns `IF_JOYPAD` on a high-to-low edge of a selected line.
    pub fn write(&mut self, value: u8) -> u8 {
        let before = self.low_nibble();
        self.select = value & 0x30;
        let after = self.low_nibble();
        self.last_low = after;
        if before & !after != 0 { IF_JOYPAD } else { 0 }
    }

    pub fn set(&mut self, buttons: Buttons) -> u8 {
        let before = self.low_nibble();
        self.pressed = buttons;
        let after = self.low_nibble();
        self.last_low = after;
        if before & !after != 0 { IF_JOYPAD } else { 0 }
    }

    /// The 2x4 matrix the select lines expose, pressed bits low.
    fn low_nibble(&self) -> u8 {
        let mut rows = 0x0F;
        if self.select & 0x10 == 0 {
            rows &= !(self.pressed.0 & 0x0F);
        }
        if self.select & 0x20 == 0 {
            rows &= !(self.pressed.0 >> 4);
        }
        rows & 0x0F
    }

    pub fn save_state(&self, w: &mut StateWriter) {
        w.u8(self.pressed.0);
        w.u8(self.select);
        w.u8(self.last_low);
    }

    pub fn load_state(&mut self, r: &mut StateReader) -> Result<(), StateError> {
        self.pressed = Buttons(r.u8()?);
        self.select = r.u8()?;
        self.last_low = r.u8()?;
        Ok(())
    }
}

impl Default for Joypad {
    fn default() -> Self {
        Joypad::new()
    }
}
