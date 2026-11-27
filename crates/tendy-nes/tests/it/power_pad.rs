//! The Power Pad on a controller port. The expected bit order below is typed out from
//! NESdev's "Power Pad" page (https://www.nesdev.org/wiki/Power_Pad, "Output ($4016/$4017 read)"),
//! not taken from the device's own tables, so a transposed button in the device fails here:
//!
//! * bit 3 (L): "Serial data from buttons 2, 1, 5, 9, 6, 10, 11, 7", one per read, 8 reads;
//! * bit 4 (H): "Serial data from buttons 4, 3, 12, 8", then H = 1 for the following 4 reads;
//! * a pressed button reads 1, and every read after the eighth returns H = 1 and L = 1.

use tendy_nes::cpu::Bus;
use tendy_nes::nes::Nes;

const L: [u8; 8] = [2, 1, 5, 9, 6, 10, 11, 7];
const H: [u8; 4] = [4, 3, 12, 8];
const D3: u8 = 0x08;
const D4: u8 = 0x10;

fn rom() -> Vec<u8> {
    let mut rom = vec![0x4E, 0x45, 0x53, 0x1A, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    rom.resize(16 + 0x4000 + 0x2000, 0);
    rom[16] = 0x4C; // JMP $C000
    rom[17] = 0x00;
    rom[18] = 0xC0;
    for v in [0x3FFA, 0x3FFC, 0x3FFE] {
        rom[16 + v] = 0x00;
        rom[16 + v + 1] = 0xC0;
    }
    rom
}

fn console_with_pad(port: usize) -> Nes {
    let mut nes = Nes::new(&rom()).expect("build console");
    nes.bus.attach_power_pad(port);
    nes
}

/// What NESdev says read `i` (0-based) returns on bits 3/4 when exactly the buttons in `held`
/// (numbered 1..=12) are down.
fn expected(i: usize, held: &[u8]) -> u8 {
    let d3 = L.get(i).is_none_or(|b| held.contains(b));
    let d4 = H.get(i).is_none_or(|b| held.contains(b));
    (u8::from(d3) * D3) | (u8::from(d4) * D4)
}

fn set(nes: &mut Nes, port: usize, held: &[u8]) {
    let mask = held.iter().fold(0u16, |m, b| m | 1 << (b - 1));
    nes.bus.set_power_pad(port, mask);
}

fn stream(nes: &mut Nes, port: usize, reads: usize) -> Vec<u8> {
    nes.bus.write(0x4016, 1);
    nes.bus.write(0x4016, 0);
    (0..reads)
        .map(|_| nes.bus.read(0x4016 + port as u16) & (D3 | D4))
        .collect()
}

#[test]
fn every_one_of_the_twelve_buttons_lands_on_its_nesdev_bit() {
    for port in 0..2usize {
        for button in 1..=12u8 {
            let mut nes = console_with_pad(port);
            set(&mut nes, port, &[button]);
            let got = stream(&mut nes, port, 12);
            let want: Vec<u8> = (0..12).map(|i| expected(i, &[button])).collect();
            assert_eq!(got, want, "port {} button {button}", port + 1);
            // The button shows up exactly once among the 8 real positions, on the one line that
            // carries it.
            let hits: usize = got[..8]
                .iter()
                .zip(0..)
                .map(|(b, i)| usize::from(*b != expected(i, &[])))
                .sum();
            assert_eq!(hits, 1, "button {button} must flip exactly one read");
        }
    }
}

#[test]
fn nothing_pressed_reads_zero_for_eight_reads_then_ones() {
    let mut nes = console_with_pad(1);
    let got = stream(&mut nes, 1, 12);
    assert_eq!(&got[..4], &[0, 0, 0, 0]);
    assert_eq!(
        &got[4..8],
        &[D4; 4],
        "H reads 1 from the fifth read on; L stays 0 to the eighth"
    );
    let want: Vec<u8> = (0..12).map(|i| expected(i, &[])).collect();
    assert_eq!(got, want);
    assert_eq!(
        got[8..],
        [D3 | D4; 4],
        "after the eighth read both lines read 1"
    );
}

#[test]
fn all_twelve_held_reads_ones_throughout() {
    let mut nes = console_with_pad(1);
    let all: Vec<u8> = (1..=12).collect();
    set(&mut nes, 1, &all);
    assert_eq!(stream(&mut nes, 1, 12), vec![D3 | D4; 12]);
}

#[test]
fn the_strobe_records_and_a_release_after_it_is_not_seen() {
    let mut nes = console_with_pad(1);
    set(&mut nes, 1, &[2]);
    nes.bus.write(0x4016, 1);
    nes.bus.write(0x4016, 0);
    set(&mut nes, 1, &[]);
    // Button 2 was down at the strobe: the first L read is 1 even though it is up now.
    assert_eq!(nes.bus.read(0x4017) & D3, D3);
    // The next strobe records the released pad.
    nes.bus.write(0x4016, 1);
    nes.bus.write(0x4016, 0);
    assert_eq!(nes.bus.read(0x4017) & D3, 0);
}

#[test]
fn strobe_held_high_keeps_answering_the_first_pair() {
    let mut nes = console_with_pad(1);
    set(&mut nes, 1, &[2, 4]);
    nes.bus.write(0x4016, 1);
    for _ in 0..5 {
        assert_eq!(
            nes.bus.read(0x4017) & (D3 | D4),
            D3 | D4,
            "buttons 2 and 4 on every read"
        );
    }
}

#[test]
fn a_port_without_a_mat_is_untouched_by_the_other_ports_mat() {
    let mut nes = console_with_pad(1);
    nes.bus.pads[0].set_buttons(0x01);
    nes.bus.write(0x4016, 1);
    nes.bus.write(0x4016, 0);
    assert_eq!(
        nes.bus.read(0x4016) & 0x01,
        0x01,
        "port 1 is still a joypad: A reads 1"
    );
}
