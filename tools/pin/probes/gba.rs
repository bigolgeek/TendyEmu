//! Game Boy Advance pin (DESIGN §5.6).
//!
//! There is no public GBA test ROM, so the probe builds one: a cartridge header plus ARM code
//! that switches the LCD to mode 3, fills the bitmap, and starts square channel 1. It then runs
//! it like any other image. The image is code, never a file in the repo.

mod common;

use common::{AudioHash, Report, fnv1a64, hex};
use gb::gba::Gba;
use gb::gba::cart::Cartridge;
use gb::system::System;

const SAMPLE_RATE: u32 = 48_000;

fn main() {
    let mut report = Report::new();
    report.run("gba/minimal");
    let mut machine =
        Gba::new(Cartridge::new(image()).expect("the probe's own GBA image is a cartridge"));
    machine.set_sample_rate(SAMPLE_RATE);
    let mut audio = Some(AudioHash::new());
    run(&mut machine, &mut audio, 60);
    report.pin("frame@60", hex(machine.frame_checksum()));
    run(&mut machine, &mut audio, 540);
    report.pin("frame@600", hex(machine.frame_checksum()));
    report.pin(
        "audio",
        hex(audio.as_ref().expect("audio was recorded").value()),
    );
    report.pin(
        "audio_peak",
        format!("{:.6}", audio.as_ref().expect("audio was recorded").peak()),
    );
    let blob = machine.save_state();
    report.pin("state@600", hex(fnv1a64(&blob)));
    report.pin("state_len@600", blob.len());
    roundtrip(&mut report, &mut machine, 600);
    report.finish();
}

fn run(machine: &mut Gba, audio: &mut Option<AudioHash>, frames: u64) {
    let mut buffer = [0.0f32; 8192];
    for _ in 0..frames {
        machine.run_frame();
        if let Some(audio) = audio.as_mut() {
            loop {
                let written = machine.drain_audio(&mut buffer);
                if written == 0 {
                    break;
                }
                audio.feed(&buffer[..written * 2]);
                if written < buffer.len() / 2 {
                    break;
                }
            }
        }
    }
}

fn roundtrip(report: &mut Report, machine: &mut Gba, at: u64) {
    let blob = machine.save_state();
    for _ in 0..60 {
        machine.run_frame();
    }
    let expected = machine.frame_checksum();
    match machine.load_state(&blob) {
        Ok(()) => {
            for _ in 0..60 {
                machine.run_frame();
            }
            let value = if machine.frame_checksum() == expected {
                "ok"
            } else {
                "mismatch"
            };
            report.pin(format!("roundtrip@{at}"), value);
        }
        Err(why) => report.pin(format!("roundtrip@{at}"), format!("load refused: {why:?}")),
    }
}

/// The cartridge image. `$C0` is where the header's branch lands.
fn image() -> Vec<u8> {
    let mut rom = vec![0u8; 0x200];
    // ARM `B` from $08000000 to $080000C0.
    rom[0..4].copy_from_slice(&0xEA00_002Eu32.to_le_bytes());
    rom[0xA0..0xA8].copy_from_slice(b"TENDY\0\0\0");
    rom[0xAC..0xB0].copy_from_slice(b"TPIN");
    rom[0xB0..0xB2].copy_from_slice(b"00");
    rom[0xB2] = 0x96;
    let mut complement: u8 = 0;
    for &byte in &rom[0xA0..=0xBC] {
        complement = complement.wrapping_sub(byte);
    }
    rom[0xBD] = complement.wrapping_sub(0x19);

    let code: [u32; 22] = [
        0xE59F0050, // ldr r0, [pc, #0x50]  r0 = 0x04000000
        0xE59F1050, // ldr r1, [pc, #0x50]  r1 = 0x0403 (mode 3, BG2 on)
        0xE5801000, // str r1, [r0]          DISPCNT
        0xE59F104C, // ldr r1, [pc, #0x4c]  r1 = 0x0080
        0xE5801084, // str r1, [r0, #0x84]   SOUNDCNT_X = enable
        0xE59F1048, // ldr r1, [pc, #0x48]  r1 = 0x0002
        0xE1C018B2, // strh r1, [r0, #0x82]  SOUNDCNT_H = PSG 100%
        0xE59F1044, // ldr r1, [pc, #0x44]  r1 = 0xf7f7
        0xE1C018B0, // strh r1, [r0, #0x80]  SOUNDCNT_L = volume 7/7, all voices
        0xE59F1040, // ldr r1, [pc, #0x40]  r1 = 0xc080 (duty 2, volume 12)
        0xE1C016B2, // strh r1, [r0, #0x62]  SOUND1CNT_H
        0xE59F103C, // ldr r1, [pc, #0x3c]  r1 = 0x8700 (period $700, trigger)
        0xE1C016B4, // strh r1, [r0, #0x64]  SOUND1CNT_X
        0xE59F2038, // ldr r2, [pc, #0x38]  r2 = 0x06000000 (VRAM)
        0xE59F3038, // ldr r3, [pc, #0x38]  r3 = 0x7c1f (first colour)
        0xE3A04000, // mov r4, #0
        0xE7823104, // str r3, [r2, r4, lsl #2]  two pixels per word
        0xE2844001, // add r4, r4, #1
        0xE2833001, // add r3, r3, #1
        0xE3540C4B, // cmp r4, #0x4b00 (240 * 160 / 2 pixels)
        0x1AFFFFFA, // bne fill
        0xEAFFFFFE, // b .
    ];
    for (index, word) in code.iter().enumerate() {
        let at = 0xC0 + index * 4;
        rom[at..at + 4].copy_from_slice(&word.to_le_bytes());
    }
    let literals: [u32; 9] = [
        0x0400_0000,
        0x0000_0403,
        0x0000_0080,
        0x0000_0002,
        0x0000_F7F7,
        0x0000_C080,
        0x0000_8700,
        0x0600_0000,
        0x0000_7C1F,
    ];
    for (index, word) in literals.iter().enumerate() {
        let at = 0x118 + index * 4;
        rom[at..at + 4].copy_from_slice(&word.to_le_bytes());
    }
    rom
}
