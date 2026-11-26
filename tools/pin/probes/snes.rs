//! SNES pins (DESIGN §5.6): a public test ROM and the refusal text for every `$FFD6` value.

mod common;

use std::path::Path;

use common::{AudioHash, Report, fnv1a64, hex, read, rom, test_roms};
use snes::cart::Cartridge;
use snes::machine::Machine;
use snes::system::System;

const SAMPLE_RATE: u32 = 48_000;
/// One of `snes-test-roms`' PPU diagnostics. The suite holds no ROM that plays music, so the
/// audio pin records the DSP's idle output; `audio_peak` says how loud that is.
const TEST_ROM: &str = "snes-test-roms/inidisp_hammer_0f.sfc";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(root) = test_roms() else { return };
    if args.iter().any(|arg| arg == "--survey") {
        survey(&root);
        return;
    }
    let mut report = Report::new();
    test_rom(&root, &mut report);
    refusals(&mut report);
    report.finish();
}

fn machine(path: &Path) -> Machine {
    let bytes = read(path);
    let cart = Cartridge::new(bytes).unwrap_or_else(|why| {
        panic!(
            "{} is not a cartridge this core accepts: {why}",
            path.display()
        )
    });
    Machine::new(cart)
}

fn drain(machine: &mut Machine, audio: &mut AudioHash) {
    let mut buffer = [0.0f32; 8192];
    loop {
        let frames = machine.drain_audio(&mut buffer);
        if frames == 0 {
            break;
        }
        audio.feed(&buffer[..frames * 2]);
        if frames < buffer.len() / 2 {
            break;
        }
    }
}

fn advance(machine: &mut Machine, frames: u64, mut audio: Option<&mut AudioHash>) {
    for _ in 0..frames {
        machine.run_frame();
        if let Some(audio) = audio.as_deref_mut() {
            drain(machine, audio);
        }
    }
}

fn test_rom(root: &Path, report: &mut Report) {
    if rom(root, TEST_ROM).is_none() {
        return;
    }
    report.run("snes/test-rom");
    let mut machine = machine(&root.join(TEST_ROM));
    machine.set_sample_rate(SAMPLE_RATE);
    let mut audio = AudioHash::new();
    advance(&mut machine, 60, Some(&mut audio));
    report.pin("frame@60", hex(machine.frame_checksum()));
    advance(&mut machine, 540, Some(&mut audio));
    report.pin("frame@600", hex(machine.frame_checksum()));
    advance(&mut machine, 2400, Some(&mut audio));
    report.pin("frame@3000", hex(machine.frame_checksum()));
    report.pin("audio", hex(audio.value()));
    report.pin("audio_peak", format!("{:.6}", audio.peak()));
    let blob = machine.save_state();
    report.pin("state@3000", hex(fnv1a64(&blob)));
    report.pin("state_len@3000", blob.len());
    roundtrip(report, &mut machine, 3000);
}

fn roundtrip(report: &mut Report, machine: &mut Machine, at: u64) {
    let blob = machine.save_state();
    advance(machine, 60, None);
    let expected = machine.frame_checksum();
    match machine.load_state(&blob) {
        Ok(()) => {
            advance(machine, 60, None);
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

/// One pin per `$FFD6` value: the core's own refusal sentence, or `ok` for a board v1 runs.
fn refusals(report: &mut Report) {
    report.run("snes/refusals");
    for byte in 0u8..=255 {
        let rom = image(byte);
        let value = match Cartridge::new(rom) {
            Ok(_) => "ok".to_owned(),
            Err(refusal) => refusal.to_string(),
        };
        report.pin(format!("ffd6_{byte:02x}"), value);
    }
}

/// A 32 KiB LoROM with a valid header, the `$FFD6` byte set to `chipset`.
fn image(chipset: u8) -> Vec<u8> {
    const AT: usize = 0x7FC0;
    let mut rom = vec![0xEAu8; 0x8000];
    rom[AT..AT + 0x15].fill(b' ');
    rom[AT..AT + 9].copy_from_slice(b"TENDY-PIN");
    rom[AT + 0x15] = 0x30; // LoROM, fast
    rom[AT + 0x16] = chipset;
    rom[AT + 0x17] = 0x05; // 32 KiB
    rom[AT + 0x18] = 0x00;
    rom[AT + 0x19] = 0x01; // USA, NTSC
    rom[AT - 1] = 0; // $FFBF subclass byte
    for vector in [0x2A, 0x2E, 0x3C] {
        rom[AT + vector..AT + vector + 2].copy_from_slice(&0x8000u16.to_le_bytes());
    }
    rom[AT + 0x1C..AT + 0x20].fill(0);
    let sum = rom
        .iter()
        .fold(0u16, |sum, &byte| sum.wrapping_add(u16::from(byte)));
    rom[AT + 0x1E..AT + 0x20].copy_from_slice(&sum.to_le_bytes());
    rom[AT + 0x1C..AT + 0x1E].copy_from_slice(&(sum ^ 0xFFFF).to_le_bytes());
    rom
}

/// `--survey`: which unpacked test ROMs make sound? Used once to choose `TEST_ROM`; kept so the
/// choice can be checked again.
fn survey(root: &Path) {
    let folder = root.join("snes-test-roms");
    let mut paths: Vec<_> = std::fs::read_dir(&folder)
        .unwrap_or_else(|why| panic!("cannot read {}: {why}", folder.display()))
        .filter_map(|entry| {
            let path = entry.ok()?.path();
            (path.extension()?.to_str()? == "sfc").then_some(path)
        })
        .collect();
    paths.sort();
    for path in paths {
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        let cart = match Cartridge::new(read(&path)) {
            Ok(cart) => cart,
            Err(why) => {
                println!("survey: {name}: refused: {why}");
                continue;
            }
        };
        let mut machine = Machine::new(cart);
        machine.set_sample_rate(SAMPLE_RATE);
        let mut audio = AudioHash::new();
        advance(&mut machine, 600, Some(&mut audio));
        let (nonzero, samples) = audio.nonzero();
        println!(
            "survey: {name}: audio={} peak={:.6} nonzero={nonzero}/{samples}",
            hex(audio.value()),
            audio.peak()
        );
    }
}
