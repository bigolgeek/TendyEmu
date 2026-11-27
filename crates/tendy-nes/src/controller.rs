/// Standard NES joypad button bits, in the order the shift register reports them (A first).
pub const BUTTON_A: u8 = 0x01;
pub const BUTTON_B: u8 = 0x02;
pub const BUTTON_SELECT: u8 = 0x04;
pub const BUTTON_START: u8 = 0x08;
pub const BUTTON_UP: u8 = 0x10;
pub const BUTTON_DOWN: u8 = 0x20;
pub const BUTTON_LEFT: u8 = 0x40;
pub const BUTTON_RIGHT: u8 = 0x80;

/// A real NES D-pad physically cannot hold UP+DOWN or LEFT+RIGHT at once.
/// If an opposing pair arrives together, drop both bits of that pair;
/// every other bit is untouched.
pub fn sanitize_dpad(buttons: u8) -> u8 {
    let mut clean = buttons;
    if clean & BUTTON_UP != 0 && clean & BUTTON_DOWN != 0 {
        clean &= !(BUTTON_UP | BUTTON_DOWN);
    }
    if clean & BUTTON_LEFT != 0 && clean & BUTTON_RIGHT != 0 {
        clean &= !(BUTTON_LEFT | BUTTON_RIGHT);
    }
    clean
}

/// A standard joypad: the frontend sets `buttons`; the CPU strobes and shifts it out through $4016/$4017.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Controller {
    pub buttons: u8,
    shift: u8,
    strobe: bool,
}

impl Controller {
    /// Assign buttons through the D-pad restriction: opposing directions that a
    /// real NES D-pad cannot press together are dropped before they are stored.
    pub fn set_buttons(&mut self, buttons: u8) {
        self.buttons = sanitize_dpad(buttons);
    }

    /// Write to $4016: bit 0 is the strobe. While it is high the shift register keeps reloading.
    pub fn write(&mut self, val: u8) {
        self.strobe = val & 0x01 != 0;
        if self.strobe {
            self.shift = self.buttons;
        }
    }

    /// Read one bit (A, B, Select, Start, Up, Down, Left, Right, then 1s).
    /// While strobe is high every read returns the A button.
    pub fn read(&mut self) -> u8 {
        if self.strobe {
            return self.buttons & 0x01;
        }
        let bit = self.shift & 0x01;
        self.shift = (self.shift >> 1) | 0x80;
        bit
    }

    /// The bit a read would return now, without clocking the shift register: the value a read on a
    /// /OE line that is still asserted from the previous cycle produces.
    pub fn peek(&self) -> u8 {
        if self.strobe {
            self.buttons & 0x01
        } else {
            self.shift & 0x01
        }
    }

    /// Writes every `Controller` field in declaration order: buttons, shift, strobe.
    pub fn save_state(&self, w: &mut crate::state::StateWriter) {
        w.u8(self.buttons);
        w.u8(self.shift);
        w.bool(self.strobe);
    }

    /// Mirror of `save_state`.
    pub fn load_state(
        &mut self,
        r: &mut crate::state::StateReader,
    ) -> Result<(), crate::state::StateError> {
        self.buttons = r.u8()?;
        self.shift = r.u8()?;
        self.strobe = r.bool()?;
        Ok(())
    }
}

/// A Vaus (Arkanoid) paddle: one fire button and one knob, read as a serial value on the two
/// lines a joypad uses for Select and Start.
///
/// The knob is a potentiometer whose measurement is latched into an 8-bit 4021 shift register.
/// Every /OE pulse — a read of `$4016` for port 1 or `$4017` for port 2 — shifts one bit out,
/// most-significant first, and the data comes back **inverted** (NESdev, "Arkanoid controller").
/// The byte the CPU sees is `xxxD Bxxx`: /D4 is the serial data and /D3 the fire button, 1 while
/// it is held. Turning the knob right raises the measurement; 0x80 is its centre.
///
/// Two simplifications, neither visible to software that strobes and then reads 8 bits:
///
/// * The conversion is instantaneous on the strobe, where hardware latches its result about a
///   millisecond later (so software reads the *previous* conversion). A knob cannot move inside
///   one frame, and every game strobes once per frame before reading, so both models hand back
///   the same byte.
/// * The ninth bit is the shift register's serial input, held at 1 here, so a read past the
///   eighth returns 0 on /D4 — what hardware does between conversions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Paddle {
    /// The knob's 8-bit measurement: 0x00 fully left, 0x80 centre, 0xFF fully right.
    pub position: u8,
    /// The fire button, reported on /D3.
    pub fire: bool,
    /// The latched measurement, shifted out MSB first and refilled with ones.
    shift: u8,
}

impl Default for Paddle {
    /// A paddle at the knob's centre with the button up: what a freshly plugged-in controller
    /// reports, and a value inside the `$62-$F2` band Arkanoid and its tests accept.
    fn default() -> Self {
        Paddle {
            position: 0x80,
            fire: false,
            shift: 0x80,
        }
    }
}

impl Paddle {
    /// A paddle whose knob sits at `position` (`0x00` left .. `0xFF` right).
    pub fn at(position: u8) -> Paddle {
        Paddle {
            position,
            ..Paddle::default()
        }
    }

    /// Write to `$4016`: bit 0 is the strobe. Asserting it starts a conversion, which this model
    /// completes at once by latching the knob into the shift register.
    pub fn write(&mut self, val: u8) {
        if val & 0x01 != 0 {
            self.shift = self.position;
        }
    }

    /// One `/OE` pulse: the fire button on /D3, the next inverted data bit on /D4.
    pub fn read(&mut self) -> u8 {
        let byte = self.peek();
        self.shift = (self.shift << 1) | 0x01;
        byte
    }

    /// The byte a read would return now, without clocking the shift register: the value a read on
    /// an /OE line still asserted from the previous cycle produces.
    pub fn peek(&self) -> u8 {
        let data = (self.shift & 0x80) >> 7;
        let fire = if self.fire { BUTTON_START } else { 0 };
        fire | (!data & 0x01) << 4
    }

    /// Writes every `Paddle` field in declaration order: position, fire, shift.
    pub fn save_state(&self, w: &mut crate::state::StateWriter) {
        w.u8(self.position);
        w.bool(self.fire);
        w.u8(self.shift);
    }

    /// Mirror of `save_state`.
    pub fn load_state(
        &mut self,
        r: &mut crate::state::StateReader,
    ) -> Result<(), crate::state::StateError> {
        self.position = r.u8()?;
        self.fire = r.bool()?;
        self.shift = r.u8()?;
        Ok(())
    }
}

/// A Zapper light gun: a photodiode on `/D3` and the trigger on `/D4`, read from the port the gun
/// is plugged into (NESdev, "Zapper", https://www.nesdev.org/wiki/Zapper).
///
/// The gun is two open-drain outputs and nothing else. There is no shift register inside it, so a
/// read returns the same byte however often the CPU reads the port, and both lines idle high
/// through the port's pull-ups: an idle gun over a dark pixel reads `0x18`. That is why
/// [`read`](Zapper::read) takes the light sample as an argument — the sense has to be taken from
/// the picture at the moment of the read, which only the machine holding the framebuffer can do.
///
/// Both lines are pulled *low* when asserted: the trigger while it is held, the light sense while
/// the beam is over a bright pixel. The gun sinks current through its phototransistor instead of
/// driving the line high, so software aims by looking for `0` in bit 3 and fires on `0` in bit 4.
///
/// **No PPU-timing coupling.** Real hardware only sees light for the few scanlines the beam spends
/// under the gun's narrow field of view, which is why a gun game reads the port right after the
/// beam passes and why a bright band on the wrong scanline can fool it. This model does not
/// simulate the beam: a read samples the pixel the gun points at in the last rendered frame,
/// whatever the PPU is drawing at that dot. Every Zapper game flashes the whole screen white to
/// aim, so the two agree for them; the integration test in `tests/zapper.rs` pins the
/// approximation so nobody mistakes it for beam accuracy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Zapper {
    /// The picture pixel the gun is aimed at, 0..=255 across.
    pub x: u16,
    /// The picture pixel the gun is aimed at, 0..=239 down.
    pub y: u16,
    /// The trigger: held by the player, which pulls `/D4` low.
    pub trigger: bool,
}

/// `/D3` of a Zapper port: the light sense, high while no light reaches the photodiode.
pub const ZAPPER_LIGHT_SENSE: u8 = 0x08;
/// `/D4` of a Zapper port: the trigger, high while it is released.
pub const ZAPPER_TRIGGER: u8 = 0x10;

/// Luma above which a picture pixel counts as bright for the Zapper's sensor.
///
/// The sensor sits behind an infrared filter and is pointed at a white flash, not at scenery: a gun
/// that answered to any lit pixel would find "light" everywhere in a platformer and every shot
/// would hit. 96 is the threshold that separates the 2C02 palette's blacks and near-blacks (`$0F`
/// and `$00`, luma 0 and 84) from its whites and midtones (`$30`, luma 236; `$10`, luma 150), so
/// the black/white flash a gun game uses is unambiguous while a grey backdrop is not mistaken for
/// the flash. It is a choice, not a hardware number — the real sensor's response curve is analogue
/// and undocumented — and [`is_bright`] is the only place it is applied, so a different gun with a
/// different sensor only has to change this one constant.
pub const BRIGHT_LUMA: u8 = 96;

/// BT.601 luma of an RGB colour, 0..=255: `0.299 R + 0.587 G + 0.114 B`, in integers so the answer
/// is exact and identical on every platform.
pub fn luma(rgb: (u8, u8, u8)) -> u8 {
    let (r, g, b) = rgb;
    ((299 * u32::from(r) + 587 * u32::from(g) + 114 * u32::from(b)) / 1000) as u8
}

/// Whether the Zapper's sensor trips on a pixel of this colour: its luma is above
/// [`BRIGHT_LUMA`].
pub fn is_bright(rgb: (u8, u8, u8)) -> bool {
    luma(rgb) > BRIGHT_LUMA
}

impl Default for Zapper {
    /// A gun aimed at the middle of the picture with the trigger released: what a freshly plugged-in
    /// gun points at, and a valid aim for a game that reads the port before the player moves the
    /// mouse.
    fn default() -> Self {
        Zapper {
            x: 128,
            y: 120,
            trigger: false,
        }
    }
}

impl Zapper {
    /// A gun aimed at `(x, y)` (`0..=255` across, `0..=239` down) with the trigger released.
    pub fn at(x: u16, y: u16) -> Zapper {
        Zapper {
            x,
            y,
            trigger: false,
        }
    }

    /// The byte its port returns when the picture pixel under the gun is `bright`.
    ///
    /// `bright` is the light sense the caller sampled at the moment of the read (see [`Zapper`]
    /// for why it cannot live in the device), and the byte is the same on every read because
    /// nothing in a Zapper clocks. The two lines are the only bits the gun drives; the rest of the
    /// port reads 0, and software masks `0x18` anyway.
    pub fn read(&self, bright: bool) -> u8 {
        let light = if bright { 0 } else { ZAPPER_LIGHT_SENSE };
        let trigger = if self.trigger { 0 } else { ZAPPER_TRIGGER };
        light | trigger
    }

    /// Writes every `Zapper` field in declaration order: x, y, trigger.
    pub fn save_state(&self, w: &mut crate::state::StateWriter) {
        w.u16(self.x);
        w.u16(self.y);
        w.bool(self.trigger);
    }

    /// Mirror of `save_state`.
    pub fn load_state(
        &mut self,
        r: &mut crate::state::StateReader,
    ) -> Result<(), crate::state::StateError> {
        self.x = r.u16()?;
        self.y = r.u16()?;
        self.trigger = r.bool()?;
        Ok(())
    }
}

/// The NES Four Score: an adapter in both controller ports that multiplexes four pads onto the two
/// serial lines (NESdev, "Four player adapters", section "Four Score").
///
/// Each port's `D0` reports 24 bits after a strobe: the port's own pad (8 bits, A first), then the
/// second pad (8 bits), then a signature byte that lets software detect the adapter, then 1s.
///
/// ```text
/// $4016: pad 1 (bits 0-7), pad 3 (8-15), then 0,0,0,1,0,0,0,0 (16-23; "$10" shifted in MSB first)
/// $4017: pad 2 (bits 0-7), pad 4 (8-15), then 0,0,1,0,0,0,0,0 (16-23; "$20" shifted in MSB first)
/// 24 and beyond: 1
/// ```
///
/// Pads 1 and 2 stay in `SysBus::pads`; this device holds pads 3 and 4 and each port's read
/// counter, and the bus asks it first on a port whose adapter is attached. Not modelled: the
/// 2-controller/4-controller switch (the device is always in 4-controller mode) and the A/B turbo
/// switches.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FourScore {
    /// Pads 3 (port 1) and 4 (port 2).
    pub pads: [Controller; 2],
    /// Reads since the last strobe on each port, saturating at 24.
    reads: [u8; 2],
}

/// The signature bits a Four Score appends on each port, in read order (bit 0 first).
const FOUR_SCORE_SIGNATURE: [u8; 2] = [0x08, 0x04];

impl FourScore {
    /// A `$4016` write: strobes pads 3 and 4; while strobe is high the report is held at its start.
    pub fn write(&mut self, val: u8) {
        self.pads[0].write(val);
        self.pads[1].write(val);
        if val & 0x01 != 0 {
            self.reads = [0, 0];
        }
    }

    /// One read on `port` (0 or 1) with `main` the port's own joypad (pad 1 or 2).
    pub fn read(&mut self, port: usize, main: &mut Controller) -> u8 {
        if main.strobe {
            return main.read();
        }
        let n = self.reads[port];
        let bit = match n {
            0..=7 => main.read(),
            8..=15 => self.pads[port].read(),
            16..=23 => (FOUR_SCORE_SIGNATURE[port] >> (n - 16)) & 1,
            _ => 1,
        };
        self.reads[port] = (n + 1).min(24);
        bit
    }

    /// The bit a read would return now, without clocking anything.
    pub fn peek(&self, port: usize, main: &Controller) -> u8 {
        if main.strobe {
            return main.peek();
        }
        let n = self.reads[port];
        match n {
            0..=7 => main.peek(),
            8..=15 => self.pads[port].peek(),
            16..=23 => (FOUR_SCORE_SIGNATURE[port] >> (n - 16)) & 1,
            _ => 1,
        }
    }

    /// Writes pad 3, pad 4, then the two read counters.
    pub fn save_state(&self, w: &mut crate::state::StateWriter) {
        self.pads[0].save_state(w);
        self.pads[1].save_state(w);
        w.u8(self.reads[0]);
        w.u8(self.reads[1]);
    }

    /// Mirror of `save_state`.
    pub fn load_state(
        &mut self,
        r: &mut crate::state::StateReader,
    ) -> Result<(), crate::state::StateError> {
        self.pads[0].load_state(r)?;
        self.pads[1].load_state(r)?;
        self.reads[0] = r.u8()?.min(24);
        self.reads[1] = r.u8()?.min(24);
        Ok(())
    }
}

/// The order in which a Power Pad reports its twelve buttons on `$4016`/`$4017` bit 3 (NESdev,
/// "Power Pad", https://www.nesdev.org/wiki/Power_Pad, "Output ($4016/$4017 read)"): the page
/// lists "Serial data from buttons 2, 1, 5, 9, 6, 10, 11, 7" for the low line (bit 3), one per read.
/// Values are button numbers 1..=12 as printed on side B.
pub const POWER_PAD_D3_ORDER: [u8; 8] = [2, 1, 5, 9, 6, 10, 11, 7];

/// The same page's "Serial data from buttons 4, 3, 12, 8" for the high line (bit 4): four reads,
/// after which the line reads 1 ("following 4 bits read as H=1").
pub const POWER_PAD_D4_ORDER: [u8; 4] = [4, 3, 12, 8];

/// A Power Pad (Family Fun Fitness) in a controller port: twelve pressure sensors, side B
/// numbering 1..=12 in a 3x4 grid, read serially on `/D3` and `/D4` of the port it is plugged
/// into (NESdev, "Power Pad", https://www.nesdev.org/wiki/Power_Pad). The protocol here is built
/// from that page alone.
///
/// A `$4016` write with bit 0 high records the buttons; with bit 0 low again the pad is read
/// "two at a time": each read returns one bit of the 8-long sequence [`POWER_PAD_D3_ORDER`] on
/// bit 3 and, for the first four reads, one bit of [`POWER_PAD_D4_ORDER`] on bit 4. A pressed
/// button reads 1. From the ninth read on both lines read 1. While the strobe is held high the
/// pad keeps re-recording, so every read returns the first pair (buttons 2 and 4), the way a
/// joypad answers with its A button.
///
/// Not modelled: the sensors' analogue settle time. The pad is NES-port-only; the Famicom's
/// Family Trainer Mat uses a different, matrix-scanned protocol that this device does not speak.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PowerPad {
    /// The buttons held now: bit `n - 1` is button `n` (1..=12). Bits 12..=15 are ignored.
    pub buttons: u16,
    /// The buttons recorded by the last strobe.
    latched: u16,
    /// Reads since the last strobe, saturating at 8.
    reads: u8,
    strobe: bool,
}

impl PowerPad {
    /// Whether `button` (1..=12) is set in `mask`.
    fn pressed(mask: u16, button: u8) -> u8 {
        u8::from(mask >> (button - 1) & 1 != 0)
    }

    /// Set or clear one button, numbered 1..=12 as printed on side B. Other numbers do nothing.
    pub fn set_button(&mut self, button: u8, down: bool) {
        if (1..=12).contains(&button) {
            let bit = 1u16 << (button - 1);
            if down {
                self.buttons |= bit;
            } else {
                self.buttons &= !bit;
            }
        }
    }

    /// A `$4016` write: bit 0 is the strobe. While high the pad records the buttons continuously
    /// and the read position stays at the start.
    pub fn write(&mut self, val: u8) {
        self.strobe = val & 0x01 != 0;
        if self.strobe {
            self.latched = self.buttons & 0x0FFF;
            self.reads = 0;
        }
    }

    /// The byte a read would return now: bit 3 and bit 4, nothing else.
    pub fn peek(&self) -> u8 {
        let mask = if self.strobe {
            self.buttons & 0x0FFF
        } else {
            self.latched
        };
        let n = if self.strobe {
            0
        } else {
            usize::from(self.reads)
        };
        let d3 = POWER_PAD_D3_ORDER
            .get(n)
            .map_or(1, |&b| Self::pressed(mask, b));
        let d4 = POWER_PAD_D4_ORDER
            .get(n)
            .map_or(1, |&b| Self::pressed(mask, b));
        d3 << 3 | d4 << 4
    }

    /// One `/OE` pulse: the next pair of bits, clocking the sequence on.
    pub fn read(&mut self) -> u8 {
        let byte = self.peek();
        if !self.strobe {
            self.reads = (self.reads + 1).min(8);
        }
        byte
    }

    /// Writes buttons, latched buttons, read count, strobe.
    pub fn save_state(&self, w: &mut crate::state::StateWriter) {
        w.u16(self.buttons);
        w.u16(self.latched);
        w.u8(self.reads);
        w.bool(self.strobe);
    }

    /// Mirror of `save_state`.
    pub fn load_state(
        &mut self,
        r: &mut crate::state::StateReader,
    ) -> Result<(), crate::state::StateError> {
        self.buttons = r.u16()? & 0x0FFF;
        self.latched = r.u16()? & 0x0FFF;
        self.reads = r.u8()?.min(8);
        self.strobe = r.bool()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_buttons_in_order() {
        let mut c = Controller {
            buttons: BUTTON_A | BUTTON_START | BUTTON_RIGHT,
            ..Default::default()
        };

        c.write(1);
        c.write(0);

        let bits: Vec<u8> = (0..8).map(|_| c.read()).collect();
        assert_eq!(bits, vec![1, 0, 0, 1, 0, 0, 0, 1]);
    }

    #[test]
    fn reads_one_after_eight_bits() {
        let mut c = Controller::default();

        c.write(1);
        c.write(0);

        for _ in 0..8 {
            c.read();
        }

        assert_eq!(c.read(), 1);
        assert_eq!(c.read(), 1);
    }

    #[test]
    fn strobe_high_returns_a() {
        let mut c = Controller {
            buttons: BUTTON_A,
            ..Default::default()
        };

        c.write(1);

        assert_eq!(c.read(), 1);
        assert_eq!(c.read(), 1);

        c.buttons = BUTTON_B;
        assert_eq!(c.read(), 0);
    }

    #[test]
    fn strobe_latches_current_buttons() {
        let mut c = Controller {
            buttons: BUTTON_B,
            ..Default::default()
        };

        c.write(1);
        c.write(0);

        c.buttons = BUTTON_A;

        assert_eq!(c.read(), 0);
        assert_eq!(c.read(), 1);
    }

    #[test]
    fn opposing_dpad_pairs_are_cleared() {
        assert_eq!(sanitize_dpad(BUTTON_UP | BUTTON_DOWN), 0);
        assert_eq!(sanitize_dpad(BUTTON_LEFT | BUTTON_RIGHT), 0);
        assert_eq!(
            sanitize_dpad(BUTTON_UP | BUTTON_DOWN | BUTTON_LEFT | BUTTON_RIGHT),
            0,
        );
        assert_eq!(sanitize_dpad(BUTTON_A | BUTTON_UP | BUTTON_DOWN), BUTTON_A);
    }

    #[test]
    fn single_dpad_directions_are_preserved() {
        assert_eq!(sanitize_dpad(BUTTON_UP), BUTTON_UP);
        assert_eq!(sanitize_dpad(BUTTON_DOWN), BUTTON_DOWN);
        assert_eq!(sanitize_dpad(BUTTON_LEFT), BUTTON_LEFT);
        assert_eq!(sanitize_dpad(BUTTON_RIGHT), BUTTON_RIGHT);
    }

    #[test]
    fn diagonal_dpad_directions_are_preserved() {
        assert_eq!(
            sanitize_dpad(BUTTON_UP | BUTTON_LEFT),
            BUTTON_UP | BUTTON_LEFT
        );
        assert_eq!(
            sanitize_dpad(BUTTON_UP | BUTTON_RIGHT),
            BUTTON_UP | BUTTON_RIGHT
        );
        assert_eq!(
            sanitize_dpad(BUTTON_DOWN | BUTTON_LEFT),
            BUTTON_DOWN | BUTTON_LEFT
        );
        assert_eq!(
            sanitize_dpad(BUTTON_DOWN | BUTTON_RIGHT),
            BUTTON_DOWN | BUTTON_RIGHT
        );
    }

    #[test]
    fn set_buttons_denies_opposite_directions() {
        let mut c = Controller::default();
        c.set_buttons(BUTTON_UP | BUTTON_DOWN | BUTTON_A);
        assert_eq!(c.buttons, BUTTON_A);
        c.set_buttons(BUTTON_LEFT | BUTTON_RIGHT | BUTTON_START);
        assert_eq!(c.buttons, BUTTON_START);
        c.set_buttons(BUTTON_UP | BUTTON_RIGHT);
        assert_eq!(c.buttons, BUTTON_UP | BUTTON_RIGHT);
    }
    #[test]
    fn paddle_reports_the_fire_button_on_d3() {
        let mut p = Paddle::default();
        p.write(1);
        assert_eq!(p.read() & BUTTON_START, 0);
        p.fire = true;
        p.write(1);
        assert_eq!(p.read() & BUTTON_START, BUTTON_START);
    }

    #[test]
    fn paddle_shifts_the_knob_out_msb_first_inverted() {
        let mut p = Paddle::at(0xA5);
        p.write(1);
        // 0xA5 = 1010_0101; every bit comes back inverted on /D4.
        let bits: Vec<u8> = (0..8).map(|_| u8::from(p.read() & 0x10 != 0)).collect();
        assert_eq!(bits, vec![0, 1, 0, 1, 1, 0, 1, 0]);
    }

    #[test]
    fn paddle_reads_zero_after_the_eighth_bit() {
        let mut p = Paddle::at(0xFF);
        p.write(1);
        for _ in 0..8 {
            p.read();
        }
        assert_eq!(p.read() & 0x10, 0);
        assert_eq!(p.read() & 0x10, 0);
    }

    #[test]
    fn paddle_strobe_latches_the_knob() {
        let mut p = Paddle::at(0x62);
        p.write(1);
        p.write(0);
        p.position = 0xF2;
        // The register holds the conversion the strobe started, not the knob's new position.
        let bits: Vec<u8> = (0..8).map(|_| u8::from(p.read() & 0x10 != 0)).collect();
        assert_eq!(bits, vec![1, 0, 0, 1, 1, 1, 0, 1]);
        p.write(1);
        let bits: Vec<u8> = (0..8).map(|_| u8::from(p.read() & 0x10 != 0)).collect();
        assert_eq!(bits, vec![0, 0, 0, 0, 1, 1, 0, 1]);
    }

    #[test]
    fn paddle_peek_does_not_clock() {
        let mut p = Paddle::at(0x80);
        p.write(1);
        assert_eq!(p.peek(), p.peek());
        assert_eq!(p.read(), 0);
        assert_eq!(p.read(), 0x10);
    }

    #[test]
    fn paddle_state_round_trips() {
        let mut p = Paddle::at(0x3C);
        p.fire = true;
        p.write(1);
        p.read();
        let mut w = crate::state::StateWriter::new();
        p.save_state(&mut w);
        let bytes = w.finish();
        let mut r = crate::state::StateReader::new(&bytes);
        let mut q = Paddle::default();
        q.load_state(&mut r).unwrap();
        assert_eq!(q.position, 0x3C);
        assert!(q.fire);
        assert_eq!(q.shift, p.shift);
    }

    #[test]
    fn an_idle_zapper_reads_0x18() {
        let gun = Zapper::default();
        assert_eq!(
            gun.read(false),
            0x18,
            "no light, trigger up: both lines idle high"
        );
        assert_eq!(
            gun.read(true),
            ZAPPER_TRIGGER,
            "light pulls only the light sense low"
        );
    }

    #[test]
    fn zapper_trigger_reads_low_while_held() {
        let mut gun = Zapper::at(100, 100);
        assert_eq!(
            gun.read(false) & ZAPPER_TRIGGER,
            ZAPPER_TRIGGER,
            "a released trigger idles high"
        );
        gun.trigger = true;
        assert_eq!(
            gun.read(false) & ZAPPER_TRIGGER,
            0,
            "a held trigger pulls /D4 low"
        );
        assert_eq!(
            gun.read(false),
            ZAPPER_LIGHT_SENSE,
            "and leaves the light sense alone"
        );
    }

    #[test]
    fn zapper_light_sense_reads_low_over_a_bright_pixel() {
        let gun = Zapper::at(100, 100);
        assert_eq!(
            gun.read(true) & ZAPPER_LIGHT_SENSE,
            0,
            "light pulls /D3 low"
        );
        assert_eq!(
            gun.read(false) & ZAPPER_LIGHT_SENSE,
            ZAPPER_LIGHT_SENSE,
            "dark leaves it high"
        );
        let mut held = Zapper::at(100, 100);
        held.trigger = true;
        assert_eq!(
            held.read(true),
            0,
            "light and a held trigger: both lines low"
        );
    }

    #[test]
    fn the_zapper_never_clocks() {
        let gun = Zapper::at(10, 20);
        let first = gun.read(true);
        for _ in 0..8 {
            assert_eq!(
                gun.read(true),
                first,
                "a Zapper has no shift register to shift out"
            );
        }
    }

    #[test]
    fn bright_is_a_luma_above_the_threshold() {
        assert_eq!(luma((0, 0, 0)), 0);
        assert_eq!(luma((255, 255, 255)), 255);
        // $00, the palette's grey "black", and $0F its true black are dark to the sensor...
        assert!(!is_bright((84, 84, 84)));
        assert!(!is_bright((0, 0, 0)));
        // ...while $30 (the white every gun game flashes) and $10's light grey are bright.
        assert!(is_bright((236, 238, 236)));
        assert!(is_bright((152, 150, 152)));
        // The threshold is exclusive: luma 96 itself is still dark, 97 is bright.
        assert_eq!(luma((96, 96, 96)), BRIGHT_LUMA);
        assert!(!is_bright((96, 96, 96)));
        assert!(is_bright((97, 97, 97)));
    }

    #[test]
    fn zapper_state_round_trips() {
        let gun = Zapper {
            x: 33,
            y: 44,
            trigger: true,
        };
        let mut w = crate::state::StateWriter::new();
        gun.save_state(&mut w);
        let data = w.finish();
        let mut back = Zapper::default();
        let mut r = crate::state::StateReader::new(&data);
        back.load_state(&mut r).expect("load");
        assert_eq!(back, gun);
    }
}
