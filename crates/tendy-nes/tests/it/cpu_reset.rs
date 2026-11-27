//! CPU reset ROMs (H6000 + reset press): the cpu_reset ROMs write $81 to $6000 to mean
//! "waiting for the reset button", then report their real result once it is pressed.

use tendy_nes::nes::Nes;

use crate::common::rom_file;

/// Run `name` until it waits for a reset ($6000 == 0x81, limit 600 frames), run 10 more
/// frames (more than 100 ms), press reset, then run until $6000 < 0x80 (limit 600 frames).
/// Returns the final status and the text at $6004.
fn run_with_reset(name: &str) -> (u8, String) {
    let mut nes = Nes::new(&rom_file(name)).unwrap();
    for _ in 0..600 {
        nes.run_frame();
        if nes.bus.cart_mut().cpu_read(0x6000) == 0x81 {
            break;
        }
    }
    for _ in 0..10 {
        nes.run_frame();
    }
    nes.reset();
    for _ in 0..600 {
        nes.run_frame();
        if nes.bus.cart_mut().cpu_read(0x6000) < 0x80 {
            break;
        }
    }
    let mut text = String::new();
    let mut addr = 0x6004;
    while nes.bus.cart_mut().cpu_read(addr) != 0 && addr < 0x7000 {
        text.push(char::from(nes.bus.cart_mut().cpu_read(addr)));
        addr += 1;
    }
    (nes.bus.cart_mut().cpu_read(0x6000), text)
}

#[test]
fn ram_after_reset() {
    let (status, text) = run_with_reset("ram_after_reset.nes");
    assert_eq!(status, 0, "{text}");
}

#[test]
fn registers() {
    let (status, text) = run_with_reset("registers.nes");
    assert_eq!(status, 0, "{text}");
}
