//! The frozen test helpers. A lane that needs another helper adds it privately
//! inside its own test file, not here.

#![allow(dead_code)]

use std::path::PathBuf;
use tendy_snes::cart::{Board, Cartridge};
use tendy_snes::joypad::Buttons;
use tendy_snes::machine::Machine;

/// A LoROM image of `bytes` bytes (rounded up to 32 KiB) with a valid header, `code` at `$00:8000`.
pub fn synth_lorom(bytes: usize, code: &[u8]) -> Vec<u8> {
    synth_cart(Board::LoRom, bytes, 0x00, 0, code)
}

/// A HiROM image of `bytes` bytes with a valid header and `code` at `$C0:0000`.
pub fn synth_hirom(bytes: usize, code: &[u8]) -> Vec<u8> {
    synth_cart(Board::HiRom, bytes, 0x00, 0, code)
}

/// The same, with a chosen board, chipset byte and SRAM size code.
pub fn synth_cart(board: Board, bytes: usize, chipset: u8, ram_code: u8, code: &[u8]) -> Vec<u8> {
    let size = image_size(board, bytes);
    let at = header_offset(board);
    let mut rom = vec![0xEAu8; size];

    let program = entry(code);
    let entry_at = match board {
        Board::LoRom => 0,
        Board::HiRom | Board::ExHiRom => 0x8000,
    };
    rom[entry_at..entry_at + program.len()].copy_from_slice(&program);

    // Title: `SNES-TEST` in a 21-byte field padded with spaces.
    rom[at..at + 0x15].fill(b' ');
    let title = b"SNES-TEST";
    rom[at..at + title.len()].copy_from_slice(title);

    rom[at + 0x15] = map_mode_of(board) | 0x30; // bits 7-6 zero, bit 5 set, bit 4 = fast
    rom[at + 0x16] = chipset;
    rom[at + 0x17] = size_code(size);
    rom[at + 0x18] = ram_code;
    rom[at + 0x19] = 0x01; // USA, NTSC
    rom[at + 0x1B] = 0;
    // `$FFBF`, just before the header: the subclass byte a real board carries.
    rom[at - 1] = 0;

    // Vectors: NMI, IRQ and RESET all point at the reset entry.
    for vector in [0x2A, 0x2E, 0x3C] {
        rom[at + vector..at + vector + 2].copy_from_slice(&0x8000u16.to_le_bytes());
    }

    rom[at + 0x1C..at + 0x20].fill(0);
    let sum = rom_checksum(&rom, at);
    rom[at + 0x1E..at + 0x20].copy_from_slice(&sum.to_le_bytes());
    rom[at + 0x1C..at + 0x1E].copy_from_slice(&(sum ^ 0xFFFF).to_le_bytes());
    rom
}

/// Bars `code` with a `jmp` to the entry and ends in `bra *`; the image's own fill is `$EA` (NOP).
pub fn entry(code: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(code.len() + 5);
    out.extend_from_slice(&[0x4C, 0x03, 0x80]); // jmp $8003
    out.extend_from_slice(code);
    out.extend_from_slice(&[0x80, 0xFE]); // bra *
    out
}

/// Runs `frames` frames from a fresh machine, returns the running frame checksum.
pub fn run_frames(rom: Vec<u8>, frames: u64) -> u64 {
    run_frames_with(rom, frames, |_| Buttons::NONE)
}

/// Runs with a button script: `keys(frame)` is called once per frame.
pub fn run_frames_with(rom: Vec<u8>, frames: u64, keys: impl Fn(u64) -> Buttons) -> u64 {
    let cart = Cartridge::new(rom).expect("the synthesized cartridge loads");
    let mut machine = Machine::new(cart);
    for frame in 0..frames {
        machine.set_buttons(keys(frame));
        machine.run_frame();
    }
    machine.frame_checksum()
}

/// The frame's checksum on its own, for a test that wants one picture.
pub fn frame_checksum(m: &Machine) -> u64 {
    m.last_frame_checksum()
}

/// A scratch directory under the test target, so nothing is written into the repo.
pub fn scratch_dir(name: &str) -> PathBuf {
    let mut dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
    dir.push(name);
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// The image's size: a power of two, at least one 32 KiB bank (64 KiB for the HiROMs).
fn image_size(board: Board, bytes: usize) -> usize {
    let min = match board {
        Board::LoRom => 0x8000,
        Board::HiRom => 0x1_0000,
        // The ExHiROM header sits at $40FFC0, so the image must reach past it.
        Board::ExHiRom => 0x80_0000,
    };
    bytes.next_power_of_two().max(min)
}

fn header_offset(board: Board) -> usize {
    match board {
        Board::LoRom => 0x7FC0,
        Board::HiRom => 0xFFC0,
        Board::ExHiRom => 0x40FFC0,
    }
}

fn map_mode_of(board: Board) -> u8 {
    match board {
        Board::LoRom => 0,
        Board::HiRom => 1,
        Board::ExHiRom => 5,
    }
}

fn size_code(size: usize) -> u8 {
    ((size / 1024).trailing_zeros()) as u8
}

/// §8.2's sum, with the four checksum bytes counted as `FF FF 00 00`.
fn rom_checksum(rom: &[u8], at: usize) -> u16 {
    let mut sum: u32 = 0;
    for (i, b) in rom.iter().enumerate() {
        sum += match i.wrapping_sub(at) {
            0x1C | 0x1D => 0xFF,
            0x1E | 0x1F => 0x00,
            _ => u32::from(*b),
        };
    }
    sum as u16
}
