//! Game Boy, Game Boy Color and Super Game Boy pins (DESIGN §5.6).
//!
//! Two acid2 pictures, Blargg's `cpu_instrs`, the SGB border composition, and (when the owner
//! has supplied the cartridge image) SGB sound.

mod common;

use std::path::Path;

use common::{AudioHash, Report, fnv1a64, fnv1a64_words, hex, read, rom, test_roms};
use gb::system::{SgbScreen, System};
use gb::{Cartridge, Machine};

/// 48 kHz is what every audio pin is measured at.
const SAMPLE_RATE: u32 = 48_000;

fn main() {
    let Some(root) = test_roms() else { return };
    let mut report = Report::new();

    dmg_acid2(&root, &mut report);
    cpu_instrs(&root, &mut report);
    cgb_acid2(&root, &mut report);
    sgb(&root, &mut report);
    sgb_sound(&root, &mut report);
    report.finish();
}

fn machine(root: &Path, name: &str) -> Machine {
    let path = root.join(name);
    let bytes = read(&path);
    let cart = Cartridge::new(bytes)
        .unwrap_or_else(|why| panic!("{} is not a Game Boy cartridge: {why}", path.display()));
    Machine::new(cart)
}

fn drain(machine: &mut dyn System, audio: &mut AudioHash) {
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

fn run(machine: &mut dyn System, audio: &mut Option<AudioHash>, frames: u64) {
    for _ in 0..frames {
        machine.run_frame();
        if let Some(audio) = audio.as_mut() {
            drain(machine, audio);
        }
    }
}

fn frame_pin(report: &mut Report, machine: &dyn System, at: u64) {
    report.pin(format!("frame@{at}"), hex(machine.frame_checksum()));
}

fn state_pins(report: &mut Report, machine: &mut dyn System, at: u64) {
    let blob = machine.save_state();
    report.pin(format!("state@{at}"), hex(fnv1a64(&blob)));
    report.pin(format!("state_len@{at}"), blob.len());
}

/// Saves at `at`, runs 60 frames, loads, runs 60 again: the two frame checksums must match.
fn roundtrip(report: &mut Report, machine: &mut dyn System, at: u64) {
    let blob = machine.save_state();
    run(machine, &mut None, 60);
    let expected = machine.frame_checksum();
    match machine.load_state(&blob) {
        Ok(()) => {
            run(machine, &mut None, 60);
            let value = if machine.frame_checksum() == expected {
                "ok".to_owned()
            } else {
                "mismatch".to_owned()
            };
            report.pin(format!("roundtrip@{at}"), value);
        }
        Err(why) => report.pin(format!("roundtrip@{at}"), format!("load refused: {why:?}")),
    }
}

fn dmg_acid2(root: &Path, report: &mut Report) {
    if rom(root, "dmg-acid2.gb").is_none() {
        return;
    }
    report.run("gb/dmg-acid2");
    let mut machine = machine(root, "dmg-acid2.gb");
    machine.set_sample_rate(SAMPLE_RATE);
    run(&mut machine, &mut None, 60);
    frame_pin(report, &machine, 60);
    run(&mut machine, &mut None, 240);
    frame_pin(report, &machine, 300);
    run(&mut machine, &mut None, 300);
    frame_pin(report, &machine, 600);
    roundtrip(report, &mut machine, 600);
}

fn cpu_instrs(root: &Path, report: &mut Report) {
    if rom(root, "cpu_instrs.gb").is_none() {
        return;
    }
    report.run("gb/cpu_instrs");
    let mut machine = machine(root, "cpu_instrs.gb");
    machine.set_sample_rate(SAMPLE_RATE);
    let mut audio = Some(AudioHash::new());
    run(&mut machine, &mut audio, 600);
    frame_pin(report, &machine, 600);
    run(&mut machine, &mut audio, 2400);
    frame_pin(report, &machine, 3000);
    report.pin(
        "audio",
        hex(audio.as_ref().expect("audio was recorded").value()),
    );
    report.pin(
        "audio_peak",
        format!("{:.6}", audio.as_ref().expect("audio was recorded").peak()),
    );
    state_pins(report, &mut machine, 3000);
    roundtrip(report, &mut machine, 3000);
}

fn cgb_acid2(root: &Path, report: &mut Report) {
    if rom(root, "cgb-acid2.gbc").is_none() {
        return;
    }
    report.run("gb/cgb-acid2");
    let mut machine = machine(root, "cgb-acid2.gbc");
    machine.set_sample_rate(SAMPLE_RATE);
    run(&mut machine, &mut None, 60);
    frame_pin(report, &machine, 60);
    run(&mut machine, &mut None, 240);
    frame_pin(report, &machine, 300);
    run(&mut machine, &mut None, 300);
    frame_pin(report, &machine, 600);
    state_pins(report, &mut machine, 600);
    roundtrip(report, &mut machine, 600);
}

fn sgb(root: &Path, report: &mut Report) {
    if rom(root, "dmg-acid2.gb").is_none() {
        return;
    }
    report.run("gb/sgb");
    let path = root.join("dmg-acid2.gb");
    let cart = Cartridge::new(read(&path))
        .unwrap_or_else(|why| panic!("{} is not a Game Boy cartridge: {why}", path.display()));
    let mut machine = Machine::new_sgb(cart);
    machine.set_sample_rate(SAMPLE_RATE);
    run(&mut machine, &mut None, 600);
    // The core's own picture, without the border (the wrapper's `set_sgb_border(false)` frame).
    frame_pin(report, &machine, 600);
    // The 256x224 bordered picture the front end draws with the border on.
    let mut screen = SgbScreen::new();
    let words = screen
        .compose(&machine)
        .expect("an SGB machine composes a bordered frame");
    report.pin("frame_border@600", hex(fnv1a64_words(words)));
    state_pins(report, &mut machine, 600);
    roundtrip(report, &mut machine, 600);
}

fn sgb_sound(root: &Path, report: &mut Report) {
    let Some(sgb_cart) = rom(root, "sgb-cartridge.bin") else {
        return;
    };
    if rom(root, "cpu_instrs.gb").is_none() {
        return;
    }
    report.run("gb/sgb-sound");
    let path = root.join("cpu_instrs.gb");
    let cart = Cartridge::new(read(&path))
        .unwrap_or_else(|why| panic!("{} is not a Game Boy cartridge: {why}", path.display()));
    let mut machine = Machine::new_sgb(cart);
    machine.set_sample_rate(SAMPLE_RATE);
    if let Some(why) = machine.set_sgb_sound(read(&sgb_cart)) {
        report.skip(format!("SGB cartridge refused: {why}"));
        return;
    }
    let mut audio = Some(AudioHash::new());
    run(&mut machine, &mut audio, 1200);
    report.pin(
        "audio@1200",
        hex(audio.as_ref().expect("audio was recorded").value()),
    );
    report.pin(
        "audio_peak@1200",
        format!("{:.6}", audio.as_ref().expect("audio was recorded").peak()),
    );
    state_pins(report, &mut machine, 1200);
    roundtrip(report, &mut machine, 1200);
}
