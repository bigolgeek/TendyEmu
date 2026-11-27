//! `$4016` and `$4017` are one arm of `SysBus::access_read` now, and these tests pin what the
//! merge must keep true.
//!
//! * The shared part — the strobe flush, the /OE hold, one bit shifted per read — behaves the
//!   same on both ports, so a fix to one is a fix to the other.
//! * The genuine differences survive: the expansion port drives D1 on `$4016` and D1-D4 on `$4017`,
//!   and the open-bus mask is configurable on port 1 (`$E0` plus the machine's spare lines) while
//!   `$4017` always keeps only `$E0`.

use tendy_nes::cart::split;
use tendy_nes::console::{ConsoleConfig, Quirks};
use tendy_nes::cpu::Bus;
use tendy_nes::expansion::ExpansionDevice;
use tendy_nes::nes::{Nes, PowerOn};
use tendy_nes::state::fnv1a64;

use crate::common::loop_rom;

fn console(quirks: Quirks) -> Nes {
    let rom = loop_rom(0, 0);
    let config = ConsoleConfig::with_quirks(PowerOn::default(), quirks);
    Nes::from_rom_data_console(split(&rom).expect("parse rom"), fnv1a64(&rom), &config)
        .expect("build console")
}

/// Strobe both pads, then shift one byte out of each port: identical pads must report identical
/// bytes, in the same order.
#[test]
fn both_ports_shift_out_the_same_sequence() {
    let mut nes = console(Quirks::default());
    nes.bus.pads[0].set_buttons(0x5A);
    nes.bus.pads[1].set_buttons(0x5A);
    nes.bus.write(0x4016, 1);
    nes.bus.write(0x4016, 0);
    let one: Vec<u8> = (0..8).map(|_| nes.bus.read(0x4016) & 1).collect();
    let two: Vec<u8> = (0..8).map(|_| nes.bus.read(0x4017) & 1).collect();
    assert_eq!(one, two, "one strobe, two identical pads");
    assert!(
        one.contains(&1),
        "the sequence must not be all zeroes: {one:?}"
    );
}

/// Port 1 keeps the machine's spare bus lines (the `nes101-port` quirk adds D2); port 2 keeps `$E0`
/// and nothing else, and neither read is affected by the other's mask.
#[test]
fn the_open_bus_mask_is_per_port() {
    let mut nes = console(Quirks {
        nes101_port: true,
        ..Quirks::default()
    });
    nes.bus.write(0x4016, 1);
    nes.bus.write(0x4016, 0);

    // The last byte on the bus was all ones: each port keeps only the bits in its own mask.
    nes.bus.open_bus = 0xFF;
    let port1 = nes.bus.read(0x4016);
    assert_eq!(port1 & 0xFC, 0xE4, "nes101-port: $4016 keeps D2 and D5-D7");
    assert_eq!(
        port1 & 0x18,
        0x00,
        "$4016 leaves D3-D4 to the port, not the bus"
    );

    nes.bus.open_bus = 0xFF;
    let port2 = nes.bus.read(0x4017);
    assert_eq!(port2 & 0xE0, 0xE0, "$4017 keeps D5-D7");
    assert_eq!(port2 & 0x1E, 0x00, "$4017 keeps nothing below D5");

    let mut plain = console(Quirks::default());
    plain.bus.write(0x4016, 1);
    plain.bus.write(0x4016, 0);
    plain.bus.open_bus = 0xFF;
    assert_eq!(
        plain.bus.read(0x4016) & 0x04,
        0x00,
        "without the quirk $4016 leaves D2 alone"
    );
}

/// The expansion device drives D1 on `$4016` and D1-D4 on `$4017`: the OR-in is per port, and it is
/// applied to the pad byte rather than replacing it.
#[test]
fn the_expansion_port_drives_different_pins_per_port() {
    let mut nes = console(Quirks::default());
    nes.bus.expansion = ExpansionDevice::Fixed {
        d1_4016: 0x02,
        d4_4017: 0x1E,
        out: 0,
    };
    nes.bus.pads[0].set_buttons(0x01); // A pressed: the pad drives D0
    nes.bus.write(0x4016, 1);
    nes.bus.write(0x4016, 0);

    nes.bus.open_bus = 0x00;
    assert_eq!(
        nes.bus.read(0x4016) & 0x1F,
        0x03,
        "pad A on D0 and the device on D1"
    );
    nes.bus.open_bus = 0x00;
    assert_eq!(
        nes.bus.read(0x4017) & 0x1F,
        0x1E,
        "device D1-D4 on $4017, no pad bit"
    );
}
