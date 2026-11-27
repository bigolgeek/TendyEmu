//! The pad port: the auto-read's bit order and busy flag, the manual strobe, "no pad" and the
//! register's inversion (fullsnes "SNES Controllers I/O
//! Ports - Automatic Reading" and "- Manual Reading").

mod common;

use tendy_snes::Machine;
use tendy_snes::addr::Addr;
use tendy_snes::cart::Cartridge;
use tendy_snes::joypad::Buttons;
use tendy_snes::ppu::LineEvents;

fn fresh() -> Machine {
    let rom = common::synth_lorom(0x8000, &[0xEA]);
    let cart = Cartridge::new(rom).expect("the synthesized cartridge loads");
    Machine::new(cart)
}

/// One full auto-read, started the way the hardware starts it: the V-blank event with the joypad
/// enable set in `$4200` bit 0.
fn auto_read(machine: &mut Machine, buttons: Buttons) -> u16 {
    machine.set_buttons(buttons);
    machine.bus.io.nmitimen = 0x01;
    machine.bus.ppu.line = 225;
    machine.bus.on_line_events(LineEvents(LineEvents::VBLANK));
    u16::from_le_bytes([
        machine.bus.read(Addr::new(0, 0x4218)),
        machine.bus.read(Addr::new(0, 0x4219)),
    ])
}

#[test]
fn auto_read_bit_order_is_the_register_order() {
    // fullsnes `$4218`: bit 15 is B ("1st"), bit 4 is R ("12th"), and every button bit reads
    // "(1=Low=Pressed)" — set while the button is down. Its "Controller ID Bits" table gives a normal
    // joypad `0000` in the 13th-16th bits, so a standard pad's low nibble reads 0.
    let mut machine = fresh();
    let image = auto_read(&mut machine, Buttons::B);
    assert_eq!(
        image, 0x8000,
        "B alone sets bit 15 and nothing else: ${image:04X} (idle reads $0000)"
    );
    let image = auto_read(&mut machine, Buttons::Y);
    assert_eq!(image, 0x4000, "Y alone sets bit 14");
    let image = auto_read(&mut machine, Buttons::R);
    assert_eq!(image, 0x0010, "R alone sets bit 4, the twelfth out");
    let image = auto_read(&mut machine, Buttons(Buttons::B.0 | Buttons::A.0));
    assert_eq!(image, 0x8080, "B and A set bits 15 and 7");

    // The four ports' words, low byte first: `$4218` is JOY1L and `$4219` JOY1H. A standard pad's ID
    // nibble is 0000, so an idle low byte is 0 (fullsnes: "no buttons pressed will return 0") rather
    // than the $0F a "no pad connected" line would give.
    machine.set_buttons(Buttons::LEFT);
    machine.bus.io.nmitimen = 0x01;
    machine.bus.ppu.line = 225;
    machine.bus.on_line_events(LineEvents(LineEvents::VBLANK));
    assert_eq!(
        machine.bus.read(Addr::new(0, 0x4218)),
        0x00,
        "JOY1L holds A, X, L, R and the ID nibble; LEFT is in the high byte"
    );
    assert_eq!(
        machine.bus.read(Addr::new(0, 0x4219)),
        0x02,
        "JOY1H: LEFT sets bit 1"
    );
    let idle = auto_read(&mut machine, Buttons::NONE);
    assert_eq!(idle, 0x0000, "no buttons pressed reads 0");
    assert_eq!(
        idle & 0x000F,
        0x0000,
        "the low nibble is the controller ID: 0000 for a standard pad"
    );
}

/// The edge the cartridges compute — `(now EOR previous) AND now` — must see a press on the frame the
/// button goes down, not on the frame it comes back up. Super Mario World's `ControllerUpdate` writes
/// exactly that (`$00:8659`–`$00:8660`: `EOR $0DAC / AND $0DA4 / STA $0DA8`) and its jump code reads
/// the result with `BMI` for "B newly pressed" (`$00:D630`, `LDA byetudlrFrame / ORA axlr0000Frame /
/// BMI`), which only works if the register sets a bit for a pressed button — fullsnes `$4218`
/// "(1=Low=Pressed)", the SFC wiki "The button bits are set when the buttons are pressed".
#[test]
fn auto_read_edge_detects_a_press_on_the_press_frame() {
    let mut machine = fresh();
    let mut previous = 0u16;
    let mut edges = Vec::new();
    for buttons in [
        Buttons::NONE,
        Buttons::NONE,
        Buttons::START,
        Buttons::START,
        Buttons::NONE,
        Buttons::NONE,
    ] {
        let now = auto_read(&mut machine, buttons);
        let edge = (now ^ previous) & now;
        edges.push(edge);
        previous = now;
    }
    assert_eq!(
        edges,
        [0x0000, 0x0000, 0x1000, 0x0000, 0x0000, 0x0000],
        "START's edge lands on the press frame, bit 12, and on no other frame"
    );
}

#[test]
fn auto_read_sets_the_busy_flag_for_4224_clocks() {
    // fullsnes: the auto-read "begins between H=32.5 and H=95.5 of the first V-Blank scanline, and
    // ends 4224 master cycles later. Register $4212 bit 0 is set during this time."
    let mut machine = fresh();
    machine.bus.io.nmitimen = 0x01;
    machine.bus.ppu.line = 225;
    machine.bus.on_line_events(LineEvents(LineEvents::VBLANK));
    assert!(machine.bus.joypad.auto, "the auto-read started");
    assert_eq!(
        machine.bus.joypad.auto_timer, 4224,
        "it is 4224 clocks long"
    );
    assert_eq!(
        machine.bus.read(Addr::new(0, 0x4212)) & 0x01,
        0x01,
        "$4212 bit 0 is the busy flag"
    );
    let remaining = machine.bus.joypad.auto_timer;
    machine.bus.joypad.tick(u32::from(remaining) - 1);
    assert!(
        machine.bus.joypad.auto,
        "still busy one clock before the window closes"
    );
    machine.bus.joypad.tick(1);
    assert!(
        !machine.bus.joypad.auto,
        "not busy once the 4224 clocks are up"
    );
    assert_eq!(
        machine.bus.read(Addr::new(0, 0x4212)) & 0x01,
        0,
        "$4212 bit 0 cleared"
    );

    // Without `$4200` bit 0 there is no auto-read, so the flag never sets: fullsnes's summary of
    // the port is "Initially disabled on reset" for the joypad enable.
    let mut machine = fresh();
    machine.bus.io.nmitimen = 0x00;
    machine.bus.ppu.line = 225;
    machine.bus.on_line_events(LineEvents(LineEvents::VBLANK));
    assert!(
        !machine.bus.joypad.auto,
        "the disabled auto-read did not start"
    );
}

#[test]
fn manual_strobe_shifts_sixteen_bits() {
    // fullsnes "Manual Reading": write 1 to `$4016` (the shift register latches and holds its first
    // bit), write 0 (the register starts shifting), then read `$4016` sixteen times for bit 0.
    let mut machine = fresh();
    machine.set_buttons(Buttons(Buttons::A.0 | Buttons::START.0));
    let register = Buttons(Buttons::A.0 | Buttons::START.0).register();
    machine.bus.write(Addr::new(0, 0x4016), 0x01);
    for _ in 0..4 {
        assert_eq!(
            machine.bus.read(Addr::new(0, 0x4016)) & 0x01,
            u8::from(register & 0x8000 != 0),
            "with the strobe high the register holds its first bit"
        );
    }
    machine.bus.write(Addr::new(0, 0x4016), 0x00);
    let mut shifted = 0u16;
    for _ in 0..16 {
        shifted = (shifted << 1) | u16::from(machine.bus.read(Addr::new(0, 0x4016)) & 0x01);
    }
    assert_eq!(
        shifted, register,
        "the sixteen bits are the register image ${register:04X}, B first"
    );
    assert_eq!(
        shifted & 0x000F,
        0x0000,
        "the last four bits are the ID nibble, 0000 for a standard pad"
    );
    assert_eq!(
        shifted & 0x1000,
        0x1000,
        "START is down, so its bit is set (fullsnes: 1=Low=Pressed)"
    );

    // A seventeenth read has nothing left to shift and reads one.
    assert_eq!(machine.bus.read(Addr::new(0, 0x4016)) & 0x01, 1);
}

#[test]
fn a_second_manual_poll_reads_the_pad_again() {
    // The latch is the `$4016` **write**'s: snes.nesdev.org "Controller reading", §Manual controller
    // reading — "writing a 1, then a 0 to this register will reset the controllers". A cartridge
    // polls by writing 1, writing 0, then reading sixteen bits, and does it again next frame. If the
    // shift position is only reset by a read that lands inside the high pulse, the second poll never
    // resets at all: the position has run past sixteen, the register answers `1` on every read, and
    // the cartridge sees all twelve buttons held down for ever.
    let mut machine = fresh();
    machine.set_buttons(Buttons(Buttons::Y.0 | Buttons::LEFT.0));
    let register = Buttons(Buttons::Y.0 | Buttons::LEFT.0).register();
    for poll in 0..3 {
        machine.bus.write(Addr::new(0, 0x4016), 0x01);
        machine.bus.write(Addr::new(0, 0x4016), 0x00);
        let mut shifted = 0u16;
        for _ in 0..16 {
            shifted = (shifted << 1) | u16::from(machine.bus.read(Addr::new(0, 0x4016)) & 0x01);
        }
        assert_eq!(
            shifted, register,
            "poll {poll} must shift the same pad word ${register:04X}: the 1-then-0 write sequence \
             latches the register, and nothing else does"
        );
    }
}

#[test]
fn a_port_with_no_pad_reads_a_zero_word() {
    // v1 has one pad, so ports 2-4 have nothing on the line: no button bit can be set, and the four
    // ID bits read the same 0000 a plain pad sends (fullsnes "Controller ID Bits": "13th-16th bits
    // 0000" is shared by "no controller connected" and "normal joypad"; they differ only in the
    // extended bits 17-24, which `$4218`-`$421F` do not carry).
    let mut machine = fresh();
    machine.set_buttons(Buttons::B);
    machine.bus.io.nmitimen = 0x01;
    machine.bus.ppu.line = 225;
    machine.bus.on_line_events(LineEvents(LineEvents::VBLANK));
    for offset in [0x421A, 0x421B, 0x421C, 0x421D, 0x421E, 0x421F] {
        assert_eq!(
            machine.bus.read(Addr::new(0, offset)),
            0x00,
            "${offset:04X} is a port with no pad: no button is pressed there"
        );
    }
    // The manual port 2 register's data line reads zero — no controller, no button bit — while its
    // JOY4 line (bit 1) stays high. snes.nesdev.org "Controller reading": "If no controller is
    // connected to a port all bits should read as 0."
    assert_eq!(
        machine.bus.read(Addr::new(0, 0x4017)) & 0x01,
        0x00,
        "$4017's data line reads 0 with no pad, so no button is ever 'pressed' there"
    );
    assert_eq!(
        machine.bus.read(Addr::new(0, 0x4017)) & 0x02,
        0x02,
        "$4017's JOY4 line (bit 1) is not the data line and reads high"
    );
    // And the auto-read of port 1 still carries the pressed button.
    assert_eq!(
        machine.bus.read(Addr::new(0, 0x4219)) & 0x80,
        0x80,
        "B is pressed"
    );
}

#[test]
fn buttons_register_sets_the_pressed_bits() {
    // §2.8: "1 = pressed" in `Buttons`, and the register image is that same word: bit 15 is B down to
    // bit 4 R, with bits 3-0 the controller's ID nibble — 0000 for a standard pad (fullsnes `$4218`
    // "(1=Low=Pressed)" and "Controller ID Bits"). DESIGN.md §2.8's "pressed buttons read 0" is the
    // inverted reading this card corrects.
    assert_eq!(Buttons::NONE.register(), 0x0000);
    assert_eq!(Buttons::B.register(), 0x8000);
    assert_eq!(Buttons::ALL[0].register(), 0x8000);
    let all = Buttons(0xFFF0);
    assert_eq!(
        all.register(),
        0xFFF0,
        "all twelve pressed sets every button bit"
    );
    assert_eq!(
        all.register() & 0x000F,
        0x0000,
        "bits 3-0 are the ID nibble, never button bits"
    );
    assert!(Buttons::UP.any());
    assert!(!Buttons::NONE.any());
    assert_eq!(Buttons::ALL.len(), 12);
    // The twelve constants are in `$4218` order, bit 15 down to bit 4.
    for (i, button) in Buttons::ALL.iter().enumerate() {
        assert_eq!(
            button.0,
            1 << (15 - i),
            "Buttons::ALL[{i}] is bit {}",
            15 - i
        );
    }
}

/// The manual registers' unused bits are open bus, and only those.
///
/// fullsnes "SNES Unpredictable Things", the unused-bits table: "4016h FCh JOYA Bit7-2 are open
/// bus" and "4017h E0h JOYB Bit7-5 are open bus". `$4017`'s bits 4-2 are its own data lines, so
/// they read zero with no pad rather than repeating the data bus.
#[test]
fn the_manual_registers_take_only_their_documented_open_bus_bits() {
    let mut m = fresh();
    m.bus.mdr = 0xFC;
    m.bus.joypad.index = 0; // no button held: bit 0 is the first bit of the (zero) shift register
    m.bus.joypad.strobe = 0;
    assert_eq!(
        m.bus.read(Addr::new(0, 0x4016)),
        0xFE,
        "fullsnes: JOYA's bits 7-2 are open bus ($FC) and bits 1-0 are the port's own lines"
    );
    assert_eq!(
        m.bus.read(Addr::new(0, 0x4017)),
        0xE2,
        "fullsnes: JOYB's bits 7-5 are open bus ($E0), bit 1 floats high, bit 0 is the data line"
    );
}
