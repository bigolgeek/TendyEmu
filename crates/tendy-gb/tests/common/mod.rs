//! Frozen test helpers (DESIGN §10). Every test builds its cartridge as bytes in code, so no test
//! needs a ROM file. A test file that needs another helper adds it to its own module.

#![allow(dead_code)]

use tendy_gb::{Cartridge, Machine};

/// The title every synthesized ROM carries.
pub const TITLE: &[u8] = b"GB-TEST";
/// A synthesized ROM's size.
pub const ROM_SIZE: usize = 0x8000;
/// Where `entry` puts the code block.
pub const ENTRY_ADDR: usize = 0x0150;

/// 32 KiB ROM: header at `$0100-$014F`, `code` at `$0100`, cart type `$00`.
///
/// The header is written after the code, so pass `entry(...)` unless the block fits in `$0100-$0103`.
pub fn synth_rom(code: &[u8]) -> Vec<u8> {
    synth_rom_banked(2, 0x00, code)
}

/// Same, with `banks` x 16 KiB and a chosen type byte (`$00`, `$01..$03`, `$05/$06`, `$0F..$13`,
/// `$19..$1E`, `$FF`).
pub fn synth_rom_banked(banks: usize, cart_type: u8, code: &[u8]) -> Vec<u8> {
    let banks = banks.max(2);
    let mut rom = vec![0u8; banks * 0x4000];
    let end = code.len().min(rom.len());
    rom[..end].copy_from_slice(&code[..end]);
    write_header(&mut rom, cart_type);
    rom
}

/// Jumps `code` to `$0150` so the entry can be a plain block; pads with `$00` (NOP) and ends in an
/// infinite `jr -2` so a runaway program cannot walk into the header.
pub fn entry(code: &[u8]) -> Vec<u8> {
    let mut block = vec![0u8; ENTRY_ADDR - 0x0100];
    block[0] = 0xC3; // jp $0150
    block[1] = 0x50;
    block[2] = 0x01;
    block.extend_from_slice(code);
    block.extend_from_slice(&[0x18, 0xFE]); // jr -2
    block
}

/// The machine a synthesized ROM boots into.
pub fn machine(rom: Vec<u8>) -> Machine {
    Machine::new(Cartridge::new(rom).expect("synthesized ROM refused"))
}

/// Runs `frames` frames from a fresh machine, returns the running frame checksum.
pub fn run_frames(rom: Vec<u8>, frames: u64) -> u64 {
    let mut m = machine(rom);
    for _ in 0..frames {
        m.run_frame();
    }
    m.frame_checksum()
}

/// Runs until the serial buffer holds a NUL-terminated string or `max_frames`, returns the text.
pub fn run_serial(rom: Vec<u8>, max_frames: u64) -> String {
    let mut m = machine(rom);
    for _ in 0..max_frames {
        m.run_frame();
        if serial_output(&m).contains('\0') {
            break;
        }
    }
    serial_output(&m)
}

/// Everything the serial port has completed so far, without draining it.
pub fn serial_output(m: &Machine) -> String {
    String::from_utf8_lossy(&m.bus.serial.out).into_owned()
}

/// Header fields the synthesized ROM carries, written after the code block.
fn write_header(rom: &mut [u8], cart_type: u8) {
    let banks = rom.len() / 0x4000;
    rom[0x0134..0x0134 + TITLE.len()].copy_from_slice(TITLE);
    rom[0x0143] = 0x00; // CGB flag: DMG only
    rom[0x0146] = 0x00; // SGB flag
    rom[0x0147] = cart_type;
    rom[0x0148] = size_code(banks);
    rom[0x0149] = 0x00; // no cartridge RAM
    rom[0x014E] = 0x00;
    rom[0x014F] = 0x00;
    rom[0x014D] = header_checksum(rom);
    let global: u16 = rom
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != 0x014E && *i != 0x014F)
        .fold(0u16, |acc, (_, b)| acc.wrapping_add(*b as u16));
    rom[0x014E] = (global >> 8) as u8;
    rom[0x014F] = global as u8;
}

/// The header checksum.
fn header_checksum(rom: &[u8]) -> u8 {
    let mut x: u8 = 0;
    for &byte in &rom[0x0134..=0x014C] {
        x = x.wrapping_sub(byte).wrapping_sub(1);
    }
    x
}

/// `$0148` for a bank count, including the three 72/80/96-bank codes.
fn size_code(banks: usize) -> u8 {
    match banks {
        72 => 0x52,
        80 => 0x53,
        96 => 0x54,
        n => {
            let mut code = 0u8;
            let mut b = 2;
            while b < n && code < 8 {
                b <<= 1;
                code += 1;
            }
            code
        }
    }
}
