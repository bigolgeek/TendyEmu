//! The shared SPC700 + S-DSP through the SGB sound path's own entry point (issue #6, DESIGN §4.3).
//!
//! The `[gb/sgb-sound]` pin is skipped until the user supplies `sgb-cartridge.bin`, so the SGB
//! copy of the chip has no sound baseline at all. This probe needs no cartridge: it loads the
//! synthetic SPC700 program and its BRR through [`SgbSound::load_block`] — the direct copy
//! `SOU_TRN` makes — starts the chip at it, and pins what it plays for one second of Game Boy dots.
//!
//! Worked example, the way §5.6 defines it: `tendy-pin spc-sgb --source <source checkout>`.

mod common;

use common::{AudioHash, Report, fnv1a64, hex, spc_image, SPC_PROGRAM};
use gb::sgb::sound::{SGB_CLOCK_HZ, SgbSound};
use gb::state::StateWriter;

/// One second of Game Boy dots, in 32 slices: one slice is about 1 000 DSP samples, well inside
/// the chip's 2 048-sample staging buffer, so nothing is dropped.
const SLICES: u32 = 32;
const SLICE_DOTS: u32 = SGB_CLOCK_HZ / SLICES;

fn main() {
    let mut report = Report::new();
    report.run("spc/sgb-synthetic");

    let mut sound = SgbSound::new();
    sound.load_block(SPC_PROGRAM, &spc_image());
    sound.start(SPC_PROGRAM);

    let mut audio = AudioHash::new();
    let mut buffer = [0.0f32; 8192];
    let mut samples = 0u64;
    for _ in 0..SLICES {
        sound.tick(SLICE_DOTS);
        loop {
            let frames = sound.read_samples(&mut buffer);
            if frames == 0 {
                break;
            }
            audio.feed(&buffer[..frames * 2]);
            samples += frames as u64;
            if frames < buffer.len() / 2 {
                break;
            }
        }
    }

    report.pin("audio", hex(audio.value()));
    report.pin("audio_peak", format!("{:.6}", audio.peak()));
    report.pin("samples", samples);
    // The chip's own state, not `SgbSound`'s: the pin is the APU's, and its layout is this host's
    // (gb length-prefixes the RAM, writes `brr` as u16).
    let mut writer = StateWriter::new();
    sound.chip().save_state(&mut writer);
    let blob = writer.finish();
    report.pin("apu_state", hex(fnv1a64(&blob)));
    report.pin("apu_state_len", blob.len());
    eprintln!(
        "spc/sgb-synthetic: peak {:.6} over {samples} stereo frames, {} nonzero",
        audio.peak(),
        audio.nonzero().0
    );
    report.finish();
}
