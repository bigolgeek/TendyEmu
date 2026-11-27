//! A synthetic ROM reads the Four Score's ports on a real machine. The adapter is plugged in
//! through the frontend API and the program stores 24 reads of each port, so attach, strobe
//! fan-out, port routing and the CPU are exercised together. Report layout: NESdev, "Four player
//! adapters" (Four Score).

use tendy_nes::controller::{BUTTON_A, BUTTON_RIGHT, BUTTON_START};
use tendy_nes::cpu::Bus;
use tendy_nes::nes::Nes;

/// 16 KB NROM image, program at `$C000`, all vectors pointing at it.
fn rom() -> Vec<u8> {
    let base = 0xC000u16;
    let mut rom = vec![0x4E, 0x45, 0x53, 0x1A, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    rom.resize(16 + 0x4000 + 0x2000, 0);
    let mut p: Vec<u8> = Vec::new();
    p.extend([0xA9, 0x01, 0x8D, 0x16, 0x40]); // LDA #1 ; STA $4016
    p.extend([0xA9, 0x00, 0x8D, 0x16, 0x40]); // LDA #0 ; STA $4016
    p.extend([0xA2, 0x00]); // LDX #0
    let l = base + p.len() as u16;
    p.extend([0xAD, 0x16, 0x40, 0x29, 0x01, 0x9D, 0x00, 0x02]); // LDA $4016 ; AND #1 ; STA $0200,X
    p.extend([0xAD, 0x17, 0x40, 0x29, 0x01, 0x9D, 0x40, 0x02]); // LDA $4017 ; AND #1 ; STA $0240,X
    p.extend([0xE8, 0xE0, 28, 0xD0]); // INX ; CPX #28 ; BNE
    let rel = (l as i32 - (base as i32 + p.len() as i32 + 1)) as i8;
    p.push(rel as u8);
    let done = base + p.len() as u16;
    p.extend([0x4C, (done & 0xFF) as u8, (done >> 8) as u8]); // JMP self
    rom[16..16 + p.len()].copy_from_slice(&p);
    for v in [0x3FFA, 0x3FFC, 0x3FFE] {
        rom[16 + v] = 0x00;
        rom[16 + v + 1] = 0xC0;
    }
    rom
}

fn bits(byte: u8) -> Vec<u8> {
    (0..8).map(|i| (byte >> i) & 1).collect()
}

/// Runs the ROM and returns the 28 bits each port read.
fn run(nes: &mut Nes) -> (Vec<u8>, Vec<u8>) {
    for _ in 0..2 {
        nes.run_frame();
    }
    (
        nes.bus.ram[0x200..0x21C].to_vec(),
        nes.bus.ram[0x240..0x25C].to_vec(),
    )
}

fn expected(first: u8, second: u8, signature: [u8; 8]) -> Vec<u8> {
    let mut v = bits(first);
    v.extend(bits(second));
    v.extend(signature);
    v.extend([1, 1, 1, 1]);
    v
}

#[test]
fn attached_adapter_reports_two_pads_then_the_signature_on_each_port() {
    let mut nes = Nes::new(&rom()).expect("build console");
    nes.bus.attach_four_score();
    nes.bus.pads[0].set_buttons(BUTTON_A | BUTTON_START);
    nes.bus.pads[1].set_buttons(BUTTON_RIGHT);
    nes.bus.set_four_score_buttons(0, 0xA5);
    nes.bus.set_four_score_buttons(1, 0x4C);
    let (p1, p2) = run(&mut nes);
    // $10 / $20 shifted in MSB first: the 1 arrives 4th / 3rd of the signature byte.
    assert_eq!(
        p1,
        expected(BUTTON_A | BUTTON_START, 0xA5, [0, 0, 0, 1, 0, 0, 0, 0])
    );
    assert_eq!(p2, expected(BUTTON_RIGHT, 0x4C, [0, 0, 1, 0, 0, 0, 0, 0]));
}

#[test]
fn signature_bytes_read_as_10_and_20_when_shifted_in_msb_first() {
    let mut nes = Nes::new(&rom()).expect("build console");
    nes.bus.attach_four_score();
    let (p1, p2) = run(&mut nes);
    let fold = |s: &[u8]| s.iter().fold(0u8, |a, &b| (a << 1) | b);
    assert_eq!(fold(&p1[16..24]), 0x10);
    assert_eq!(fold(&p2[16..24]), 0x20);
}

#[test]
fn detached_adapter_leaves_plain_joypads() {
    let mut nes = Nes::new(&rom()).expect("build console");
    nes.bus.pads[0].set_buttons(BUTTON_A);
    let (p1, p2) = run(&mut nes);
    assert_eq!(
        p1,
        expected(BUTTON_A, 0xFF, [1; 8]),
        "a plain pad shifts in 1s after 8 reads"
    );
    assert_eq!(
        p2,
        vec![0, 0, 0, 0, 0, 0, 0, 0]
            .into_iter()
            .chain([1; 20])
            .collect::<Vec<u8>>()
    );
}

#[test]
fn detached_state_is_the_attached_layout_with_a_zero_presence_byte() {
    let mut nes = Nes::new(&rom()).expect("build console");
    nes.run_frame();
    let plain = nes.save_state();
    assert_eq!(&plain[4..6], &[12, 0], "STATE_VERSION 12");
    nes.bus.attach_four_score();
    let attached = nes.save_state();
    assert_eq!(
        plain.len(),
        attached.len(),
        "layout does not depend on the adapter"
    );
    let at = plain.len() - 9 - 14 - 3; // the Power Pad block (14 bytes) and expansion block (3) follow
    assert_eq!(
        &plain[..at],
        &attached[..at],
        "bytes before the block are untouched"
    );
    assert_eq!(
        plain[at + 9..],
        attached[at + 9..],
        "Power Pad and expansion blocks follow unchanged"
    );
    assert_eq!(plain[at..at + 9], [0; 9], "detached block is all zero");
    assert_eq!(attached[at], 1, "attached presence byte");
}

#[test]
fn state_round_trips_mid_read() {
    let mut nes = Nes::new(&rom()).expect("build console");
    nes.bus.attach_four_score();
    nes.bus.set_four_score_buttons(0, 0xA5);
    nes.bus.write(0x4016, 1);
    nes.bus.write(0x4016, 0);
    for _ in 0..10 {
        nes.bus.read(0x4016); // 8 of pad 1, then 2 of pad 3
    }
    let saved = nes.save_state();
    let tail: Vec<u8> = (0..16).map(|_| nes.bus.read(0x4016) & 1).collect();

    let mut fresh = Nes::new(&rom()).expect("build console");
    fresh.load_state(&saved).expect("load");
    assert!(
        fresh.bus.four_score.is_some(),
        "attachment comes back with the state"
    );
    let again: Vec<u8> = (0..16).map(|_| fresh.bus.read(0x4016) & 1).collect();
    assert_eq!(again, tail);
    // Pad 3 bits 2..7 of 0xA5 (A first), then the signature's first 0s and its 1.
    assert_eq!(&tail[..6], &bits(0xA5)[2..8]);
    assert_eq!(&tail[6..14], &[0, 0, 0, 1, 0, 0, 0, 0]);
}
