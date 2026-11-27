//! The pad port: one latch, the 16-bit auto-read that runs in V-blank, and the manual strobe
//! Sources: fullsnes "SNES Controllers I/O Ports - Automatic Reading"
//! and "- Manual Reading".
//!
//! v1 has one pad (gameport 1) and no multitap, so ports 2-4 answer "nothing connected": no button
//! bit set, and the same `0000` ID nibble a standard pad sends (fullsnes, "Controller ID Bits" -
//! a plain pad reads `0000`; the extended 17th-24th bits are not in `$4218`).

use crate::addr::Addr;
use crate::state::{StateError, StateReader, StateWriter};

/// The auto-read window: "This begins between H=32.5 and H=95.5 of the first V-Blank scanline, and
/// ends 4224 master cycles later. Register $4212 bit 0 is set during this time."
const AUTO_READ_TICKS: u16 = 4224;

/// The twelve buttons, **in `$4218` bit order**: bit 15 is B (first out), bit 4 is R (twelfth
/// out). 1 = pressed; bits 3-0 carry the controller's ID nibble and are never set here.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Buttons(pub u16);

impl Buttons {
    pub const B: Buttons = Buttons(1 << 15);
    pub const Y: Buttons = Buttons(1 << 14);
    pub const SELECT: Buttons = Buttons(1 << 13);
    pub const START: Buttons = Buttons(1 << 12);
    pub const UP: Buttons = Buttons(1 << 11);
    pub const DOWN: Buttons = Buttons(1 << 10);
    pub const LEFT: Buttons = Buttons(1 << 9);
    pub const RIGHT: Buttons = Buttons(1 << 8);
    pub const A: Buttons = Buttons(1 << 7);
    pub const X: Buttons = Buttons(1 << 6);
    pub const L: Buttons = Buttons(1 << 5);
    pub const R: Buttons = Buttons(1 << 4);
    pub const NONE: Buttons = Buttons(0);

    /// All twelve, in `Buttons` bit order: `B, Y, Select, Start, Up, Down, Left, Right, A, X, L, R`.
    pub const ALL: [Buttons; 12] = [
        Buttons::B,
        Buttons::Y,
        Buttons::SELECT,
        Buttons::START,
        Buttons::UP,
        Buttons::DOWN,
        Buttons::LEFT,
        Buttons::RIGHT,
        Buttons::A,
        Buttons::X,
        Buttons::L,
        Buttons::R,
    ];

    /// Any of the twelve held.
    pub fn any(self) -> bool {
        self.0 & 0xFFF0 != 0
    }

    /// The register image `$4218` returns: **one bit per button, set while the button is pressed**,
    /// bits 3-0 the controller's ID nibble — `0000` for a standard pad.
    ///
    /// fullsnes "SNES Controllers I/O Ports - Automatic Reading" lists every button bit as
    /// "(1=Low=Pressed)" — the register bit is 1 when the button's line is pulled low — and its
    /// "Controller ID Bits" table gives a normal joypad `0000` in the 13th-16th bits, so the low
    /// nibble of a standard pad reads 0. The SFC wiki ("Polling Controller Input", `$4218`) says the
    /// same in words: "The button bits are set when the buttons are pressed ... 0000 is the ID for a
    /// standard snes controller", and fullsnes notes of the read window that "no buttons pressed
    /// will return 0". The cartridge agrees: Super Mario World's edge detector is `EOR previous /
    /// AND current` and its jump code reads that byte with `BMI` for "B newly pressed", which is only
    /// true if a press sets the bit.
    pub fn register(self) -> u16 {
        self.0
    }
}

/// The pad port.
#[derive(Clone, Default)]
pub struct Joypad {
    /// What the host says is held down.
    pub pressed: Buttons,
    /// The four auto-read results, `$4218-$421F`, in `Buttons` bit order.
    pub shift: [u16; 4],
    /// Strobe position for the manual read: 0 = the first bit is next.
    pub index: u8,
    /// `$4212` bit 0 while the 16-bit auto-read is running.
    pub auto: bool,
    /// Master clocks left in the 4224-clock auto-read.
    pub auto_timer: u16,
    /// `$4016` bit 0 as last written: 1 latches, 0 clocks.
    pub strobe: u8,
}

impl Joypad {
    pub fn new() -> Joypad {
        // §4.5: every field zero, which reads as "no pad" until the first auto-read.
        Joypad::default()
    }

    /// `$4016` bit 0: the latch (`1`) and the shift clock (`0`).
    ///
    /// The latch belongs to the **write**, not to a read that happens while the line is still high.
    /// snes.nesdev.org "Controller reading", §Manual controller reading: *"The least significant bit
    /// in that register controls a reset signal that controllers receive, and writing a 1, then a 0 to
    /// this register will reset the controllers."* A poll is therefore `write 1`, `write 0`, sixteen
    /// reads; a machine that only resets the shift position when a read lands inside the high pulse
    /// never resets it at all, and every poll after the first reads the end-of-register `1` sixteen
    /// times — which is "all twelve buttons held down" to a cartridge.
    pub fn write_strobe(&mut self, value: u8) {
        self.strobe = value & 0x01;
        if self.strobe != 0 {
            self.index = 0;
        }
    }

    /// `$4016`/`$4017`: the manual read, one bit per read of bit 0 while the strobe is low.
    ///
    /// fullsnes "Manual Reading": reading the register "automatically generates a clock pulse";
    /// while the strobe (Out0) is high the shift register is held at its first bit, and "Be sure
    /// that Out0 in Port 4016h is zero (otherwise the shift register gets stuck on the first bit)".
    /// Bits 7-2 are open bus, bit 1 is gameport 1 pin 5 (JOY3, no pad in v1), and bit 0 is the
    /// controller's data line.
    ///
    /// Port 2 has no pad in v1, and **its data line reads zero**, not one: snes.nesdev.org
    /// "Controller reading": *"If no controller is connected to a port all bits should read as 0."*
    /// That is the same answer the automatic read gives for an empty port (`$4218`-`$421F`'s zero
    /// word), and it is what a cartridge polling both ports to see whether a controller is plugged in
    /// is asking for.
    pub fn read_manual(&mut self, addr: Addr, mdr: u8) -> u8 {
        if addr.offset() & 1 != 0 {
            // $4017: no pad on gameport 2, so the data line reads 0 and JOY4 (bit 1) reads high.
            // Bits 7-5 are open bus — fullsnes "SNES Unpredictable Things": "4017h E0h JOYB Bit7-5
            // are open bus" — so only those three come from the data bus.
            return (mdr & 0xE0) | 0x02;
        }
        if self.strobe != 0 {
            self.index = 0;
        }
        let register = self.pressed.register();
        let bit = if self.index < 16 {
            (register >> (15 - self.index)) & 1
        } else {
            1
        };
        if self.strobe == 0 {
            self.index = self.index.saturating_add(1);
        }
        (mdr & 0xFC) | (bit as u8) | 0x02
    }

    /// `$4218-$421F`: the auto-read result; always driven, never open bus (the third refinement).
    /// refinement).
    pub fn read_auto(&self, addr: Addr) -> u8 {
        let index = usize::from((addr.offset() - 0x4218) >> 1);
        let word = self.shift[index.min(3)];
        if addr.offset() & 1 == 0 {
            word as u8
        } else {
            (word >> 8) as u8
        }
    }

    /// V=225: start the 16-bit read of all four ports.
    ///
    /// Ports 2-4 have no pad in v1, and a port with no pad in it reads back a zero word — no button
    /// bit set (fullsnes: "no buttons pressed will return 0") and a zero ID nibble ("13th-16th bits
    /// 0000", the same four bits a plain pad sends).
    pub fn begin_auto_read(&mut self) {
        self.shift[0] = self.pressed.register();
        self.shift[1] = 0;
        self.shift[2] = 0;
        self.shift[3] = 0;
        self.auto = true;
        self.auto_timer = AUTO_READ_TICKS;
    }

    /// Counts `auto_timer` down, clears `auto`.
    pub fn tick(&mut self, ticks: u32) {
        if !self.auto {
            return;
        }
        let ticks = ticks.min(u32::from(u16::MAX)) as u16;
        if self.auto_timer > ticks {
            self.auto_timer -= ticks;
        } else {
            self.auto_timer = 0;
            self.auto = false;
        }
    }

    pub fn set(&mut self, buttons: Buttons) {
        self.pressed = buttons;
    }

    /// §9.2's `Joypad` rows, in the design's order.
    pub fn save_state(&self, w: &mut StateWriter) {
        w.u16(self.pressed.0);
        w.bool(self.auto);
        w.u8(self.index);
        w.u8(self.strobe);
        w.u16(self.auto_timer);
        for word in self.shift {
            w.u16(word);
        }
    }

    /// The inverse of [`Joypad::save_state`].
    pub fn load_state(&mut self, r: &mut StateReader<'_>) -> Result<(), StateError> {
        self.pressed = Buttons(r.u16()?);
        self.auto = r.bool()?;
        self.index = r.u8()?;
        self.strobe = r.u8()?;
        self.auto_timer = r.u16()?;
        for word in &mut self.shift {
            *word = r.u16()?;
        }
        Ok(())
    }
}
